//! Binary message framing with varint length-prefix.

use crate::error::FramingError;

const MAX_LENGTH_PREFIX_SIZE: usize = 5;
const NUM_BITS_TO_SHIFT: [usize; 5] = [0, 7, 14, 21, 28];

/// Binary message framing utilities.
pub struct BinaryMessageFormat;

impl BinaryMessageFormat {
    /// Encodes `payload` with a varint length prefix.
    pub fn write(payload: &[u8]) -> Vec<u8> {
        let mut len_buffer = Vec::new();
        let mut size = payload.len();

        loop {
            let mut size_part = size & 0x7f;
            size >>= 7;
            if size > 0 {
                size_part |= 0x80;
            }
            len_buffer.push(size_part as u8);
            if size == 0 {
                break;
            }
        }

        let mut output = Vec::with_capacity(len_buffer.len() + payload.len());
        output.extend_from_slice(&len_buffer);
        output.extend_from_slice(payload);
        output
    }

    /// Decodes one or more length-prefixed messages from `input`.
    pub fn parse(input: &[u8]) -> Result<Vec<Vec<u8>>, FramingError> {
        let mut result = Vec::new();
        let mut offset = 0;

        while offset < input.len() {
            let mut num_bytes = 0usize;
            let mut size = 0usize;
            let mut byte_read;

            loop {
                if offset + num_bytes >= input.len() {
                    return Err(FramingError::CannotReadSize);
                }

                byte_read = input[offset + num_bytes];
                size |= ((byte_read & 0x7f) as usize) << NUM_BITS_TO_SHIFT[num_bytes];
                num_bytes += 1;

                if num_bytes >= MAX_LENGTH_PREFIX_SIZE.min(input.len() - offset)
                    || byte_read & 0x80 == 0
                {
                    break;
                }
            }

            if byte_read & 0x80 != 0 && num_bytes < MAX_LENGTH_PREFIX_SIZE {
                return Err(FramingError::CannotReadSize);
            }

            if num_bytes == MAX_LENGTH_PREFIX_SIZE && byte_read > 7 {
                return Err(FramingError::MessageTooLarge);
            }

            if input.len() >= offset + num_bytes + size {
                result.push(input[offset + num_bytes..offset + num_bytes + size].to_vec());
            } else {
                return Err(FramingError::IncompleteMessage);
            }

            offset += num_bytes + size;
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::FramingError;

    #[test]
    fn write_empty_bytes() {
        assert_eq!(BinaryMessageFormat::write(&[]), vec![0x00]);
    }

    #[test]
    fn write_single_byte() {
        assert_eq!(BinaryMessageFormat::write(&[0x20]), vec![0x01, 0x20]);
    }

    #[test]
    fn parse_empty_input() {
        assert_eq!(
            BinaryMessageFormat::parse(&[]).unwrap(),
            Vec::<Vec<u8>>::new()
        );
    }

    #[test]
    fn parse_zero_length_message() {
        assert_eq!(
            BinaryMessageFormat::parse(&[0x00]).unwrap(),
            vec![Vec::<u8>::new()]
        );
    }

    #[test]
    fn parse_single_message() {
        assert_eq!(
            BinaryMessageFormat::parse(&[0x01, 0xff]).unwrap(),
            vec![vec![0xff]]
        );
    }

    #[test]
    fn parse_multiple_messages() {
        assert_eq!(
            BinaryMessageFormat::parse(&[0x01, 0xff, 0x01, 0x7f]).unwrap(),
            vec![vec![0xff], vec![0x7f]]
        );
    }

    #[test]
    fn parse_cannot_read_size() {
        let payloads = [
            vec![0x80],
            vec![0x02, 0x01, 0x80, 0x80],
            vec![0x07, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x80],
        ];

        for payload in payloads {
            assert_eq!(
                BinaryMessageFormat::parse(&payload),
                Err(FramingError::CannotReadSize)
            );
        }
    }

    #[test]
    fn parse_incomplete_message() {
        let payloads = [
            vec![0x07, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01],
            vec![0xff, 0xff, 0xff, 0xff, 0x07],
            vec![0x02, 0x00],
        ];

        for payload in payloads {
            assert_eq!(
                BinaryMessageFormat::parse(&payload),
                Err(FramingError::IncompleteMessage)
            );
        }
    }

    #[test]
    fn parse_message_too_large() {
        let payloads = [
            vec![0xff, 0xff, 0xff, 0xff, 0xff],
            vec![0x80, 0x80, 0x80, 0x80, 0x80],
        ];

        for payload in payloads {
            assert_eq!(
                BinaryMessageFormat::parse(&payload),
                Err(FramingError::MessageTooLarge)
            );
        }
    }
}
