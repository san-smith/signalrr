//! SignalR hub connection.

mod state;

pub use state::HubConnectionState;

use crate::negotiate::NegotiateOptions;

/// Active SignalR hub connection.
///
/// Transport, protocol, and lifecycle logic will be implemented in follow-up work.
pub struct HubConnection {
    url: String,
    #[allow(dead_code)]
    negotiate_options: NegotiateOptions,
    state: HubConnectionState,
}

impl HubConnection {
    pub(crate) fn new(url: String, negotiate_options: NegotiateOptions) -> Self {
        Self {
            url,
            negotiate_options,
            state: HubConnectionState::Disconnected,
        }
    }

    /// Hub endpoint URL.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Current connection state.
    pub fn state(&self) -> HubConnectionState {
        self.state
    }
}
