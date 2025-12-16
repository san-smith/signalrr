//! Payload compression codec for SignalR frames.
//!
//! This module provides optional compression for MessagePack payloads
//! using Brotli or Gzip. It is only available when the `compression` feature is enabled.

use crate::error::SignalRError;

/// Compression codec for SignalR message payloads.
#[derive(Clone, Debug)]
pub enum PayloadCodec {
    /// No compression (default).
    Identity,
    /// Brotli compression with specified quality (0-11).
    #[cfg(feature = "compression")]
    Brotli { quality: u32 },
    /// Gzip compression.
    #[cfg(feature = "compression")]
    Gzip,
}

impl Default for PayloadCodec {
    fn default() -> Self {
        Self::Identity
    }
}

impl PayloadCodec {
    /// Encodes (and compresses if enabled) a raw MessagePack payload.
    pub fn encode(&self, data: Vec<u8>) -> Result<Vec<u8>, SignalRError> {
        match self {
            Self::Identity => Ok(data),
            #[cfg(feature = "compression")]
            Self::Brotli { quality } => {
                use std::io::Read;
                let mut compressor = brotli::CompressorReader::new(&data[..], 4096, *quality, 22);
                let mut output = Vec::new();
                compressor.read_to_end(&mut output)?;
                Ok(output)
            }
            #[cfg(feature = "compression")]
            Self::Gzip => {
                use flate2::Compression;
                use flate2::write::GzEncoder;
                use std::io::Write;
                let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
                encoder.write_all(&data)?;
                Ok(encoder.finish()?)
            }
        }
    }

    /// Decodes (and optionally decompresses) a payload.
    pub fn decode(&self, data: Vec<u8>) -> Result<Vec<u8>, SignalRError> {
        match self {
            Self::Identity => Ok(data),
            #[cfg(feature = "compression")]
            Self::Brotli { .. } => {
                use std::io::Read;
                let mut decompressor = brotli::Decompressor::new(&data[..], 4096);
                let mut output = Vec::new();
                decompressor.read_to_end(&mut output)?;
                Ok(output)
            }
            #[cfg(feature = "compression")]
            Self::Gzip => {
                use flate2::read::GzDecoder;
                use std::io::Read;
                let mut decoder = GzDecoder::new(&data[..]);
                let mut output = Vec::new();
                decoder.read_to_end(&mut output)?;
                Ok(output)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identity_codec() {
        let codec = PayloadCodec::Identity;
        let data = b"test".to_vec();
        assert_eq!(codec.encode(data.clone()).unwrap(), data);
        assert_eq!(codec.decode(data).unwrap(), b"test".to_vec());
    }

    #[cfg(feature = "compression")]
    #[test]
    fn test_brotli_roundtrip() {
        let codec = PayloadCodec::Brotli { quality: 6 };
        let data = vec![1, 2, 3, 4, 100; 1000];
        let compressed = codec.encode(data.clone()).unwrap();
        let decompressed = codec.decode(compressed).unwrap();
        assert_eq!(data, decompressed);
    }
}
