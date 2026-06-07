//! Access token providers for HTTP negotiate and WebSocket connect.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::error::Error;

/// Async factory that supplies bearer tokens for authenticated connections.
pub type AccessTokenFactory =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<String, Error>> + Send>> + Send + Sync>;

/// Wraps an async closure as an [`AccessTokenFactory`].
pub fn factory<F, Fut>(callback: F) -> AccessTokenFactory
where
    F: Fn() -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<String, Error>> + Send + 'static,
{
    Arc::new(move || Box::pin(callback()))
}
