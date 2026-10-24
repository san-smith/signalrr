//! Connecting and exchanging messages with an in-process hub: negotiate, handshake, calls
//! in both directions, server close, keep-alive and server timeout.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use signalrr::test_support::{FakeHub, NegotiateReply, ServerConnection, TIMEOUT};
use signalrr::{
    CloseMessage, CompletionMessage, Error, HubConnection, HubConnectionBuilder,
    HubConnectionState, HubMessage, InvocationMessage,
};
use tokio::sync::mpsc;

type Builder = fn(&str) -> HubConnectionBuilder;

const PROTOCOLS: [(&str, Builder); 3] = [
    ("json", |url| {
        HubConnection::builder(url).with_json_protocol()
    }),
    ("messagepack", |url| {
        HubConnection::builder(url).with_messagepack_protocol()
    }),
    ("messagepack+brotli", |url| {
        HubConnection::builder(url).with_messagepack_brotli_protocol()
    }),
];

/// Starts `connection` against `hub` and accepts the handshake on the server side.
async fn connect(hub: &FakeHub, connection: &HubConnection) -> ServerConnection {
    let (started, server) = tokio::join!(connection.start(), async {
        let mut server = hub.accept().await;
        server.handshake().await;
        server
    });
    started.unwrap();
    server
}

async fn recv<T>(receiver: &mut mpsc::UnboundedReceiver<T>) -> T {
    tokio::time::timeout(TIMEOUT, receiver.recv())
        .await
        .expect("nothing received in time")
        .expect("sender dropped")
}

#[tokio::test]
async fn handshake_version_follows_stateful_reconnect() {
    for (name, builder) in PROTOCOLS {
        for stateful in [false, true] {
            let hub = FakeHub::start().await;
            let connection = builder(&hub.url).with_stateful_reconnect(stateful).build();

            let (started, (mut server, request)) = tokio::join!(connection.start(), async {
                let mut server = hub.accept().await;
                let request = server.handshake().await;
                (server, request)
            });
            started.unwrap();

            let version = if stateful { 2 } else { 1 };
            assert_eq!(request, json!({ "protocol": name, "version": version }));
            assert_eq!(
                hub.negotiate_requests()[0]
                    .path_and_query
                    .contains("useStatefulReconnect=true"),
                stateful,
                "{name}"
            );
            assert_eq!(server.query_value("id"), Some("token-1"));
            assert_eq!(connection.state().await, HubConnectionState::Connected);

            connection.close().await.unwrap();
            server.expect_closed().await;
        }
    }
}

#[tokio::test]
async fn send_and_invoke_reach_the_server() {
    for (name, builder) in PROTOCOLS {
        let hub = FakeHub::start().await;
        let connection = builder(&hub.url).build();
        let mut server = connect(&hub, &connection).await;

        connection
            .send("Notify", vec![json!("a").into()])
            .await
            .unwrap();
        let HubMessage::Invocation(notify) = server.recv().await else {
            panic!("{name}: expected an invocation");
        };
        assert_eq!(notify.target, "Notify");
        assert_eq!(notify.invocation_id, None);
        assert_eq!(notify.arguments.to_json().unwrap(), [json!("a")]);

        let (result, ()) = tokio::join!(
            connection.invoke("Add", vec![json!(2).into(), json!(3).into()]),
            async {
                let HubMessage::Invocation(add) = server.recv().await else {
                    panic!("{name}: expected an invocation");
                };
                assert_eq!(add.target, "Add");
                assert_eq!(add.arguments.to_json().unwrap(), [json!(2), json!(3)]);
                server
                    .send(&HubMessage::Completion(CompletionMessage {
                        headers: Default::default(),
                        invocation_id: add.invocation_id.expect("invoke has an id"),
                        error: None,
                        result: Some(json!(5)),
                    }))
                    .await;
            }
        );
        assert_eq!(result.unwrap(), json!(5), "{name}");

        connection.close().await.unwrap();
    }
}

#[derive(Debug, PartialEq, Deserialize)]
struct Point {
    x: i64,
    y: f64,
}

#[tokio::test]
async fn server_invocation_reaches_on_and_on_typed() {
    for (name, builder) in PROTOCOLS {
        let hub = FakeHub::start().await;
        let connection = builder(&hub.url).build();
        let (json_tx, mut json_rx) = mpsc::unbounded_channel();
        let (typed_tx, mut typed_rx) = mpsc::unbounded_channel();
        connection
            .on("Update", move |arguments| {
                json_tx.send(arguments).unwrap();
                None
            })
            .await;
        connection
            .on_typed("Update", move |arguments: (String, Point)| {
                typed_tx.send(arguments).unwrap();
            })
            .await;
        let mut server = connect(&hub, &connection).await;

        server
            .send(&HubMessage::Invocation(InvocationMessage {
                target: "Update".into(),
                arguments: vec![json!("a"), json!({ "x": 1, "y": 2.5 })].into(),
                ..Default::default()
            }))
            .await;

        assert_eq!(
            recv(&mut json_rx).await,
            [json!("a"), json!({ "x": 1, "y": 2.5 })],
            "{name}"
        );
        assert_eq!(
            recv(&mut typed_rx).await,
            ("a".to_string(), Point { x: 1, y: 2.5 }),
            "{name}"
        );

        connection.close().await.unwrap();
    }
}

#[tokio::test]
async fn compressed_server_invocation_reaches_on_typed() {
    let hub = FakeHub::start().await;
    let connection = HubConnection::builder(&hub.url)
        .with_messagepack_brotli_protocol()
        .build();
    let (typed_tx, mut typed_rx) = mpsc::unbounded_channel();
    connection
        .on_typed("Update", move |arguments: (String, Point)| {
            typed_tx.send(arguments).unwrap();
        })
        .await;
    let mut server = connect(&hub, &connection).await;

    let point = rmpv::Value::Map(vec![
        (rmpv::Value::from("x"), rmpv::Value::from(1)),
        (rmpv::Value::from("y"), rmpv::Value::from(2.5)),
    ]);
    server
        .send_binary(signalrr::test_support::compressed_invocation(
            "Update",
            rmpv::Value::Array(vec![rmpv::Value::from("a"), point]),
        ))
        .await;

    assert_eq!(
        recv(&mut typed_rx).await,
        ("a".to_string(), Point { x: 1, y: 2.5 })
    );
    connection.close().await.unwrap();
}

#[tokio::test]
async fn negotiate_401_retries_with_a_fresh_token() {
    let hub = FakeHub::start().await;
    hub.reply_to_negotiate(NegotiateReply::Status(401));
    let calls = Arc::new(AtomicUsize::new(0));
    let connection = HubConnection::builder(&hub.url)
        .with_json_protocol()
        .with_access_token({
            let calls = calls.clone();
            move || {
                let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
                async move { Ok(format!("access-{call}")) }
            }
        })
        .build();

    let server = connect(&hub, &connection).await;

    let authorization: Vec<_> = hub
        .negotiate_requests()
        .into_iter()
        .map(|request| request.authorization)
        .collect();
    assert_eq!(
        authorization,
        [
            Some("Bearer access-1".to_string()),
            Some("Bearer access-2".to_string())
        ]
    );
    assert_eq!(server.query_value("access_token"), Some("access-2"));
    connection.close().await.unwrap();
}

#[tokio::test]
async fn negotiate_redirect_moves_the_connection() {
    let hub = FakeHub::start().await;
    hub.reply_to_negotiate(NegotiateReply::Json(json!({
        "url": format!("{}/redirected", hub.origin),
        "accessToken": "server-token",
    })));
    let connection = HubConnection::builder(&hub.url)
        .with_json_protocol()
        .with_access_token(|| async { Ok("client-token".to_string()) })
        .build();

    let server = connect(&hub, &connection).await;

    let requests = hub.negotiate_requests();
    assert!(requests[0].path_and_query.starts_with("/hub/negotiate"));
    assert_eq!(
        requests[0].authorization.as_deref(),
        Some("Bearer client-token")
    );
    assert!(
        requests[1]
            .path_and_query
            .starts_with("/redirected/negotiate"),
        "{}",
        requests[1].path_and_query
    );
    assert_eq!(
        requests[1].authorization.as_deref(),
        Some("Bearer server-token")
    );
    assert_eq!(server.path, "/redirected");
    assert_eq!(server.query_value("access_token"), Some("server-token"));
    connection.close().await.unwrap();
}

#[tokio::test]
async fn handshake_error_fails_start() {
    let hub = FakeHub::start().await;
    let connection = HubConnection::builder(&hub.url)
        .with_json_protocol()
        .build();

    let (started, ()) = tokio::join!(connection.start(), async {
        let mut server = hub.accept().await;
        server.handshake_request().await;
        server
            .handshake_response(Some("protocol not supported"))
            .await;
    });

    let error = started.unwrap_err();
    assert!(
        matches!(&error, Error::Handshake(message) if message.contains("protocol not supported")),
        "{error}"
    );
    assert_eq!(connection.state().await, HubConnectionState::Disconnected);
}

#[tokio::test]
async fn close_from_server_closes_the_connection() {
    let hub = FakeHub::start().await;
    let connection = HubConnection::builder(&hub.url)
        .with_json_protocol()
        .build();
    let (closed_tx, mut closed_rx) = mpsc::unbounded_channel();
    connection
        .on_close(move |error| closed_tx.send(error.map(str::to_string)).unwrap())
        .await;
    let mut server = connect(&hub, &connection).await;

    server
        .send(&HubMessage::Close(CloseMessage {
            error: Some("server is shutting down".into()),
            allow_reconnect: Some(false),
        }))
        .await;

    let error = recv(&mut closed_rx).await.expect("close carries the error");
    assert!(error.contains("server is shutting down"), "{error}");
    assert_eq!(connection.state().await, HubConnectionState::Disconnected);
}

#[tokio::test]
async fn client_sends_keep_alive_pings() {
    let hub = FakeHub::start().await;
    let connection = HubConnection::builder(&hub.url)
        .with_json_protocol()
        .with_keep_alive_interval(Duration::from_millis(50))
        .build();
    let mut server = connect(&hub, &connection).await;

    for _ in 0..2 {
        assert!(matches!(server.recv_any().await, HubMessage::Ping(_)));
    }
    connection.close().await.unwrap();
}

#[tokio::test]
async fn silent_server_times_out() {
    let hub = FakeHub::start().await;
    let connection = HubConnection::builder(&hub.url)
        .with_json_protocol()
        .with_server_timeout(Duration::from_millis(300))
        .build();
    let (closed_tx, mut closed_rx) = mpsc::unbounded_channel();
    connection
        .on_close(move |error| closed_tx.send(error.map(str::to_string)).unwrap())
        .await;
    let mut server = connect(&hub, &connection).await;

    let error = recv(&mut closed_rx).await.expect("close carries the error");
    assert!(error.to_lowercase().contains("timeout"), "{error}");
    assert_eq!(connection.state().await, HubConnectionState::Disconnected);
    server.expect_closed().await;
}

/// Pings from the server keep a connection with a short server timeout alive.
#[tokio::test]
async fn server_pings_prevent_the_timeout() {
    let hub = FakeHub::start().await;
    let connection = HubConnection::builder(&hub.url)
        .with_json_protocol()
        .with_server_timeout(Duration::from_millis(300))
        .build();
    let mut server = connect(&hub, &connection).await;

    for _ in 0..6 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        server.send(&HubMessage::Ping(signalrr::PingMessage)).await;
    }
    assert_eq!(connection.state().await, HubConnectionState::Connected);
    connection.close().await.unwrap();
}

#[tokio::test]
async fn consecutive_sends_arrive_in_order() {
    let hub = FakeHub::start().await;
    let connection = HubConnection::builder(&hub.url)
        .with_messagepack_protocol()
        .build();
    let mut server = connect(&hub, &connection).await;

    for n in 0..100 {
        connection
            .send("Item", vec![json!(n).into()])
            .await
            .unwrap();
    }

    for n in 0..100 {
        let HubMessage::Invocation(item) = server.recv().await else {
            panic!("expected an invocation");
        };
        assert_eq!(item.arguments.to_json().unwrap(), [json!(n)]);
    }
    connection.close().await.unwrap();
}
