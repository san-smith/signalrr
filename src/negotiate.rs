//! Negotiation with the SignalR server.
//!
//! Before establishing a WebSocket connection, the client must perform an HTTP POST
//! request to the `/negotiate` endpoint to obtain a `connectionId`.
//!
//! This module handles the negotiation request and parses the response.

use crate::error::SignalRError;
use serde::Deserialize;
use tracing::debug;
use url::Url;

/// Response from the `/negotiate` endpoint.
#[derive(Deserialize, Debug)]
pub struct NegotiateResponse {
    #[serde(rename = "connectionId")]
    connection_id: Option<String>,
}

/// Performs the negotiate request to obtain a connection ID.
///
/// # Arguments
///
/// * `hub_url` - The base URL of the SignalR hub (e.g., `http://localhost:5000/chathub`).
///
/// # Returns
///
/// * `Ok(connection_id)` - The connection ID to use in the WebSocket URL.
/// * `Err(SignalRError)` - If the request fails or the response is invalid.
///
/// # Example
///
/// ```no_run
/// use signalrr::negotiate::negotiate;
/// use url::Url;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let hub_url = Url::parse("http://localhost:5000/chathub")?;
/// let connection_id = negotiate(&hub_url).await?;
/// println!("Connection ID: {}", connection_id);
/// # Ok(())
/// # }
/// ```
pub async fn negotiate(hub_url: &Url) -> Result<String, SignalRError> {
    let mut negotiate_url = hub_url.clone();

    // Получаем путь и нормализуем его
    let mut path = negotiate_url.path().to_string();
    if path.ends_with('/') && path.len() > 1 {
        path.pop(); // удаляем завершающий '/', но не если путь "/"
    }
    path.push_str("/negotiate");
    negotiate_url.set_path(&path);

    // Опционально: удаляем fragment (якорь), так как он не нужен для API
    negotiate_url.set_fragment(None);
    negotiate_url.set_query(hub_url.query());

    debug!("Negotiate URL: {}", negotiate_url);

    let client = reqwest::Client::new();

    match client
        .post(negotiate_url.as_str())
        .header("Accept", "application/json")
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                let raw: NegotiateResponse = response.json().await?;
                // Используем connectionId, если есть, иначе — генерируем
                Ok(raw.connection_id.unwrap_or_else(|| "local".to_string()))
            } else {
                // Если сервер вернул 404 или 405 — negotiate не требуется
                debug!("Response text: {}", response.text().await?);
                debug!("Negotiate not required, using default connection ID");
                Ok("local".to_string())
            }
        }
        Err(e) => {
            // Если запрос к /negotiate не удался — тоже пропускаем
            debug!("Negotiate failed ({}), using default connection ID", e);
            Ok("local".to_string())
        }
    }
}
