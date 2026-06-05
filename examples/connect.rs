//! Manual connectivity example against a real SignalR endpoint.
//!
//! Run with:
//! `SIGNALR_URL=https://host/signalr ACCESS_TOKEN=... cargo run --example connect`

use signalrr::HubConnection;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::var("SIGNALR_URL").expect("SIGNALR_URL must be set");
    let token = std::env::var("ACCESS_TOKEN").unwrap_or_default();

    let mut connection = HubConnection::builder(url)
        .with_access_token(move || {
            let token = token.clone();
            async move { Ok(token) }
        })
        .build();

    connection.start().await?;
    println!("Connected: {:?}", connection.connection_id());
    connection.close().await?;
    Ok(())
}
