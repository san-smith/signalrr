//! Hub connection state.

/// Current state of a [`super::HubConnection`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubConnectionState {
    Disconnected,
    Connecting,
    Connected,
    Disconnecting,
    Reconnecting,
}
