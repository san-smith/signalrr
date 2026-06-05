//! Hub protocol trait and shared input types.

use crate::error::FramingError;
use crate::hub::message::HubMessage;
use crate::transport::TransferFormat;
use thiserror::Error;

/// Errors raised while encoding or decoding hub protocol messages.
#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("invalid payload: {0}")]
    InvalidPayload(String),

    #[error("messagepack error: {0}")]
    MessagePack(String),

    #[error(transparent)]
    Json(#[from] serde_json::Error),

    #[error(transparent)]
    Framing(#[from] FramingError),
}

/// Input buffer passed to [`HubProtocol::parse_messages`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubProtocolInput<'a> {
    Text(&'a str),
    Binary(&'a [u8]),
}

/// SignalR hub protocol implementation.
pub trait HubProtocol: Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> i32;
    fn transfer_format(&self) -> TransferFormat;
    fn parse_messages(&self, input: HubProtocolInput<'_>) -> Result<Vec<HubMessage>, ProtocolError>;
    fn write_message(&self, message: &HubMessage) -> Result<Vec<u8>, ProtocolError>;
}

impl From<ProtocolError> for crate::error::Error {
    fn from(value: ProtocolError) -> Self {
        match value {
            ProtocolError::Json(error) => Self::Json(error),
            ProtocolError::Framing(error) => Self::Framing(error),
            other => Self::Protocol(other.to_string()),
        }
    }
}
