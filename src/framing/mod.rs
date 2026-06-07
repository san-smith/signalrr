//! SignalR message framing (text record separator and binary varint length-prefix).

mod binary;
mod text;

pub use binary::BinaryMessageFormat;
pub use text::{RECORD_SEPARATOR, TextMessageFormat};
