//! Text message framing with ASCII record separator (`0x1e`).

use crate::error::FramingError;

/// Record separator byte used by the SignalR text protocol.
pub const RECORD_SEPARATOR: char = '\x1e';

/// Text message framing utilities.
pub struct TextMessageFormat;

impl TextMessageFormat {
    /// Appends the record separator to `output`.
    pub fn write(output: &str) -> String {
        format!("{output}{RECORD_SEPARATOR}")
    }

    /// Splits `input` into individual messages.
    ///
    /// The input must end with a record separator.
    pub fn parse(input: &str) -> Result<Vec<String>, FramingError> {
        if input.is_empty() || !input.ends_with(RECORD_SEPARATOR) {
            return Err(FramingError::IncompleteMessage);
        }

        Ok(input
            .split(RECORD_SEPARATOR)
            .filter(|part| !part.is_empty())
            .map(str::to_owned)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_appends_record_separator() {
        assert_eq!(
            TextMessageFormat::write("lol kek"),
            format!("lol kek{RECORD_SEPARATOR}")
        );
    }

    #[test]
    fn parse_splits_messages() {
        let input = format!("lol{RECORD_SEPARATOR}kek{RECORD_SEPARATOR}");
        assert_eq!(
            TextMessageFormat::parse(&input).unwrap(),
            vec!["lol", "kek"]
        );
    }

    #[test]
    fn parse_rejects_incomplete_message() {
        assert_eq!(
            TextMessageFormat::parse("lol kek"),
            Err(FramingError::IncompleteMessage)
        );
    }
}
