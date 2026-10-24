//! Connection drops against an in-process hub: automatic reconnect, no reconnect, pending
//! invocations and stateful reconnect.

#![cfg(feature = "messagepack-brotli")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use signalrr::{
    DefaultReconnectPolicy, HubConnection, HubConnectionState, HubMessage, InvocationMessage,
};
use support::fake_hub::{FakeHub, NegotiateReply, ServerConnection, TIMEOUT};
use tokio::sync::mpsc;

/// Reconnect delays short enough for the test not to wait.
fn quick_policy(attempts: usize) -> Arc<DefaultReconnectPolicy> {
    Arc::new(DefaultReconnectPolicy::new(
        (0..attempts).map(|attempt| Duration::from_millis(10 * attempt as u64)),
    ))
}

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

/// Lifecycle callbacks as a stream of events.
async fn lifecycle_events(connection: &HubConnection) -> mpsc::UnboundedReceiver<String> {
    let (tx, rx) = mpsc::unbounded_channel();
    connection
        .on_reconnecting({
            let tx = tx.clone();
            move |_| tx.send("reconnecting".to_string()).unwrap()
        })
        .await;
    connection
        .on_reconnected({
            let tx = tx.clone();
            move |id| {
                tx.send(format!("reconnected {}", id.unwrap_or("-")))
                    .unwrap()
            }
        })
        .await;
    connection
        .on_close(move |_| tx.send("closed".to_string()).unwrap())
        .await;
    rx
}

#[tokio::test]
async fn automatic_reconnect_follows_the_policy() {
    let hub = FakeHub::start().await;
    let connection = HubConnection::builder(&hub.url)
        .with_json_protocol()
        .with_reconnect_policy(quick_policy(2))
        .build();
    let mut events = lifecycle_events(&connection).await;
    let server = connect(&hub, &connection).await;

    server.abort();
    assert_eq!(recv(&mut events).await, "reconnecting");
    let mut server = hub.accept().await;
    server.handshake().await;
    assert_eq!(recv(&mut events).await, "reconnected id-token-2");
    assert_eq!(connection.state().await, HubConnectionState::Connected);

    // Every further attempt fails: the policy runs out and the connection closes.
    for _ in 0..3 {
        hub.reply_to_negotiate(NegotiateReply::Status(500));
    }
    server.abort();
    assert_eq!(recv(&mut events).await, "reconnecting");
    assert_eq!(recv(&mut events).await, "closed");
    assert_eq!(connection.state().await, HubConnectionState::Disconnected);
    // The first connection, the successful reconnect, then one negotiate per delay.
    assert_eq!(hub.negotiate_requests().len(), 4);
}

#[tokio::test]
async fn without_automatic_reconnect_a_drop_closes_the_connection() {
    let hub = FakeHub::start().await;
    let connection = HubConnection::builder(&hub.url)
        .with_json_protocol()
        .build();
    let mut events = lifecycle_events(&connection).await;
    let server = connect(&hub, &connection).await;

    server.abort();

    assert_eq!(recv(&mut events).await, "closed");
    assert_eq!(connection.state().await, HubConnectionState::Disconnected);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(hub.negotiate_requests().len(), 1, "no reconnect attempt");
}

#[tokio::test]
async fn pending_invoke_fails_when_the_connection_drops() {
    let hub = FakeHub::start().await;
    let connection = HubConnection::builder(&hub.url)
        .with_json_protocol()
        .build();
    let mut server = connect(&hub, &connection).await;

    let (result, ()) = tokio::join!(
        tokio::time::timeout(TIMEOUT, connection.invoke("Add", vec![json!(2).into()])),
        async {
            assert!(matches!(server.recv().await, HubMessage::Invocation(_)));
            server.abort();
        }
    );

    let error = result.expect("invoke must not hang").unwrap_err();
    assert!(!error.to_string().is_empty());
}

fn invocation(target: &str, argument: i64) -> HubMessage {
    HubMessage::Invocation(InvocationMessage {
        target: target.into(),
        arguments: vec![json!(argument)].into(),
        ..Default::default()
    })
}

/// Stateful reconnect as in the official clients: the transport reopens with the same
/// connection token, without negotiate and handshake; the client sends `Sequence` and its
/// unacknowledged messages, the server resends its own; nothing is delivered twice and the
/// hub connection itself never leaves `Connected`.
#[tokio::test]
async fn stateful_reconnect_resumes_the_same_connection() {
    let hub = FakeHub::start().await;
    let connection = HubConnection::builder(&hub.url)
        .with_json_protocol()
        .with_stateful_reconnect(true)
        .build();
    let mut events = lifecycle_events(&connection).await;
    let (received_tx, mut received) = mpsc::unbounded_channel();
    connection
        .on_typed("Item", move |(n,): (i64,)| received_tx.send(n).unwrap())
        .await;
    let mut server = connect(&hub, &connection).await;

    // Server messages 1 and 2 arrive; the client acknowledges them.
    server.send(&invocation("Item", 1)).await;
    server.send(&invocation("Item", 2)).await;
    assert_eq!(recv(&mut received).await, 1);
    assert_eq!(recv(&mut received).await, 2);
    loop {
        if let HubMessage::Ack(ack) = server.recv().await {
            assert_eq!(ack.sequence_id, 2);
            break;
        }
    }

    // Client message 1 reaches the server but is not acknowledged before the drop.
    connection
        .send("Echo", vec![json!("a").into()])
        .await
        .unwrap();
    assert!(matches!(server.recv().await, HubMessage::Invocation(_)));
    server.abort();

    // While the transport is down the client keeps sending: message 2 waits in the buffer.
    connection
        .send("Echo", vec![json!("b").into()])
        .await
        .unwrap();

    let mut server = hub.accept().await;
    assert_eq!(
        server.query_value("id"),
        Some("token-1"),
        "same connection token"
    );
    assert_eq!(hub.negotiate_requests().len(), 1, "no new negotiate");
    // No handshake: the first message is the client's Sequence, then both unacknowledged
    // messages.
    server.assume_protocol("json");
    let HubMessage::Sequence(sequence) = server.recv().await else {
        panic!("expected Sequence first");
    };
    assert_eq!(sequence.sequence_id, 1);
    for expected in ["a", "b"] {
        let HubMessage::Invocation(echo) = server.recv().await else {
            panic!("expected the buffered invocation");
        };
        assert_eq!(echo.arguments.to_json().unwrap(), [json!(expected)]);
    }

    // The server resends from its message 2 (1 was acknowledged), then message 3.
    server
        .send(&HubMessage::Sequence(signalrr::SequenceMessage {
            sequence_id: 2,
        }))
        .await;
    server.send(&invocation("Item", 2)).await;
    server.send(&invocation("Item", 3)).await;
    assert_eq!(
        recv(&mut received).await,
        3,
        "message 2 is not delivered twice"
    );
    let HubMessage::Ack(ack) = server.recv().await else {
        panic!("expected an Ack for message 3");
    };
    assert_eq!(ack.sequence_id, 3);

    assert_eq!(connection.state().await, HubConnectionState::Connected);
    assert!(events.try_recv().is_err(), "no lifecycle callbacks");
    connection.close().await.unwrap();
}

/// When reopening the transport fails, the connection closes like after any drop.
#[tokio::test]
async fn failed_stateful_reconnect_closes_the_connection() {
    let hub = FakeHub::start().await;
    let connection = HubConnection::builder(&hub.url)
        .with_json_protocol()
        .with_stateful_reconnect(true)
        .build();
    let mut events = lifecycle_events(&connection).await;
    let server = connect(&hub, &connection).await;

    hub.refuse_websockets();
    server.abort();

    assert_eq!(recv(&mut events).await, "closed");
    assert_eq!(connection.state().await, HubConnectionState::Disconnected);
}

#[tokio::test]
async fn pending_invoke_survives_stateful_reconnect() {
    let hub = FakeHub::start().await;
    let connection = HubConnection::builder(&hub.url)
        .with_json_protocol()
        .with_stateful_reconnect(true)
        .build();
    let mut server = connect(&hub, &connection).await;

    let (result, ()) = tokio::join!(
        connection.invoke("Add", vec![json!(2).into(), json!(3).into()]),
        async {
            let HubMessage::Invocation(add) = server.recv().await else {
                panic!("expected an invocation");
            };
            server.abort();
            let mut server = hub.accept().await;
            server.assume_protocol("json");
            assert!(matches!(server.recv().await, HubMessage::Sequence(_)));
            // The unacknowledged invocation comes again; the server answers it now.
            assert!(matches!(server.recv().await, HubMessage::Invocation(_)));
            // The server sent nothing before, so its sequence starts at 1.
            server
                .send(&HubMessage::Sequence(signalrr::SequenceMessage {
                    sequence_id: 1,
                }))
                .await;
            server
                .send(&HubMessage::Completion(signalrr::CompletionMessage {
                    headers: Default::default(),
                    invocation_id: add.invocation_id.unwrap(),
                    error: None,
                    result: Some(json!(5)),
                }))
                .await;
        }
    );

    assert_eq!(result.unwrap(), json!(5));
    connection.close().await.unwrap();
}

/// A server that no longer knows the connection rejects the reopened transport; automatic
/// reconnect then starts a new connection with a fresh buffer that takes messages at once.
#[tokio::test]
async fn rejected_stateful_reconnect_falls_back_to_automatic_reconnect() {
    let hub = FakeHub::start().await;
    let connection = HubConnection::builder(&hub.url)
        .with_json_protocol()
        .with_stateful_reconnect(true)
        .with_reconnect_policy(quick_policy(2))
        .build();
    let mut events = lifecycle_events(&connection).await;
    let (received_tx, mut received) = mpsc::unbounded_channel();
    connection
        .on_typed("Item", move |(n,): (i64,)| received_tx.send(n).unwrap())
        .await;
    let server = connect(&hub, &connection).await;

    hub.refuse_connection_token("token-1");
    server.abort();

    assert_eq!(recv(&mut events).await, "reconnecting");
    let mut server = hub.accept().await;
    assert_eq!(server.query_value("id"), Some("token-2"));
    server.handshake().await;
    assert_eq!(recv(&mut events).await, "reconnected id-token-2");

    server.send(&invocation("Item", 1)).await;
    assert_eq!(recv(&mut received).await, 1);
    // The new connection gets no Sequence: the old buffer is gone.
    connection
        .send("Echo", vec![json!("x").into()])
        .await
        .unwrap();
    assert!(matches!(server.recv().await, HubMessage::Invocation(_)));
    connection.close().await.unwrap();
}

/// Without the server's consent there is no buffer: no Ack goes out.
#[tokio::test]
async fn no_acks_when_the_server_declines_stateful_reconnect() {
    let hub = FakeHub::start().await;
    hub.reply_to_negotiate(NegotiateReply::Json(json!({
        "negotiateVersion": 1,
        "connectionId": "id",
        "connectionToken": "token",
    })));
    let connection = HubConnection::builder(&hub.url)
        .with_json_protocol()
        .with_stateful_reconnect(true)
        .build();
    let (received_tx, mut received) = mpsc::unbounded_channel();
    connection
        .on_typed("Item", move |(n,): (i64,)| received_tx.send(n).unwrap())
        .await;
    let mut server = connect(&hub, &connection).await;

    server.send(&invocation("Item", 1)).await;
    assert_eq!(recv(&mut received).await, 1);
    // An Ack would come one second after the message.
    tokio::time::sleep(Duration::from_millis(1_500)).await;
    connection
        .send("Echo", vec![json!("x").into()])
        .await
        .unwrap();
    assert!(matches!(server.recv().await, HubMessage::Invocation(_)));
    connection.close().await.unwrap();
}

/// A full buffer holds `send` back until the server acknowledges, as in the official
/// clients.
#[tokio::test]
async fn full_buffer_waits_for_an_ack() {
    let hub = FakeHub::start().await;
    let connection = HubConnection::builder(&hub.url)
        .with_json_protocol()
        .with_stateful_reconnect(true)
        .with_stateful_reconnect_buffer_size(1)
        .build();
    let mut server = connect(&hub, &connection).await;

    let send = connection.send("Echo", vec![json!("x").into()]);
    tokio::pin!(send);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut send)
            .await
            .is_err(),
        "send waits while the buffer is full"
    );
    assert!(matches!(server.recv().await, HubMessage::Invocation(_)));
    server
        .send(&HubMessage::Ack(signalrr::AckMessage { sequence_id: 1 }))
        .await;
    tokio::time::timeout(TIMEOUT, send)
        .await
        .expect("send completes after the Ack")
        .unwrap();
    connection.close().await.unwrap();
}
