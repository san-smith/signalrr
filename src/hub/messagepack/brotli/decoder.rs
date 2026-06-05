//! Brotli decoder used by the MessagePack+Brotli hub protocol.

use std::io::Read;

use crate::hub::protocol::ProtocolError;

/// Decodes Brotli-compressed MessagePack field payloads.
pub trait BrotliDecoder: Send + Sync {
    fn decode(&self, data: &[u8]) -> Result<Vec<u8>, ProtocolError>;
}

/// Default Brotli decoder backed by the `brotli` crate.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DefaultBrotliDecoder;

impl BrotliDecoder for DefaultBrotliDecoder {
    fn decode(&self, data: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        let mut reader = brotli::Decompressor::new(data, 4096);
        let mut output = Vec::new();
        reader.read_to_end(&mut output).map_err(|error| {
            ProtocolError::InvalidPayload(format!("brotli decompression failed: {error}"))
        })?;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmpv::Value;

    fn encode_value(value: &Value) -> Vec<u8> {
        let mut buffer = Vec::new();
        rmpv::encode::write_value(&mut buffer, value).unwrap();
        buffer
    }

    #[test]
    fn decode_round_trips_msgpack_payload() {
        let original = encode_value(&Value::String("hello world".into()));
        let compressed = {
            let mut output = Vec::new();
            {
                let mut writer = brotli::CompressorWriter::new(&mut output, 4096, 11, 22);
                std::io::Write::write_all(&mut writer, &original).unwrap();
            }
            output
        };

        let decoded = DefaultBrotliDecoder.decode(&compressed).unwrap();
        assert_eq!(decoded, original);
    }
}
