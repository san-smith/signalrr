//! Arguments of a hub invocation in the representation the protocol delivered.

use std::fmt;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::de::value::SeqDeserializer;

use crate::error::Error;
use crate::hub::protocol::ProtocolError;

/// Arguments of a hub method invocation.
///
/// Incoming invocations keep the arguments as the protocol delivered them: JSON values for
/// the JSON protocol, the encoded MessagePack array for the MessagePack protocols. Use
/// [`Arguments::deserialize`] to get typed values without an intermediate
/// [`serde_json::Value`]. Cloning is cheap.
#[derive(Clone, Default)]
pub struct Arguments(Arc<Repr>);

enum Repr {
    Json(Vec<serde_json::Value>),
    /// A complete MessagePack array, checked to be well-formed.
    #[cfg(feature = "messagepack")]
    MessagePack(Vec<u8>),
}

impl Default for Repr {
    fn default() -> Self {
        Self::Json(Vec::new())
    }
}

impl Arguments {
    /// Deserializes the argument list into `T`, a tuple or a struct with one field per
    /// argument: `(String, i32)` for a method called with a string and a number, `(Update,)`
    /// for a single argument.
    ///
    /// MessagePack map keys keep their type, so a map with integer keys deserializes into
    /// `HashMap<i64, _>`. A [`serde_json::Value`] field inside `T` cannot hold such a map
    /// ("expected a string key"); use `rmpv::Value` or your own `#[serde(untagged)]` enum
    /// for dynamic fields instead.
    pub fn deserialize<T: DeserializeOwned>(&self) -> Result<T, Error> {
        match self.0.as_ref() {
            Repr::Json(values) => {
                T::deserialize(SeqDeserializer::<_, serde_json::Error>::new(values.iter()))
                    .map_err(|error| Error::InvalidArguments(error.to_string()))
            }
            #[cfg(feature = "messagepack")]
            Repr::MessagePack(bytes) => rmp_serde::from_slice(bytes)
                .map_err(|error| Error::InvalidArguments(error.to_string())),
        }
    }

    /// Returns the arguments as JSON values. MessagePack arguments are converted on each
    /// call; map keys become strings.
    pub fn to_json(&self) -> Result<Vec<serde_json::Value>, Error> {
        Ok(self.json_values()?)
    }

    pub(crate) fn json_values(&self) -> Result<Vec<serde_json::Value>, ProtocolError> {
        match self.0.as_ref() {
            Repr::Json(values) => Ok(values.clone()),
            #[cfg(feature = "messagepack")]
            Repr::MessagePack(_) => {
                crate::hub::messagepack::value_to_json_array(&self.messagepack_value()?)
            }
        }
    }

    /// Wraps an encoded MessagePack array.
    #[cfg(feature = "messagepack")]
    pub(crate) fn from_messagepack(bytes: Vec<u8>) -> Result<Self, ProtocolError> {
        let mut rest = bytes.as_slice();
        if rmp::decode::read_array_len(&mut rest).is_err() {
            return Err(ProtocolError::InvalidPayload("expected array".into()));
        }
        Ok(Self(Arc::new(Repr::MessagePack(bytes))))
    }

    /// Returns the arguments as one MessagePack array value.
    #[cfg(feature = "messagepack")]
    pub(crate) fn messagepack_value(&self) -> Result<rmpv::Value, ProtocolError> {
        match self.0.as_ref() {
            Repr::Json(values) => Ok(crate::hub::messagepack::json_array_value(values)),
            Repr::MessagePack(bytes) => {
                rmpv::decode::read_value(&mut bytes.as_slice()).map_err(|error| {
                    ProtocolError::MessagePack(format!("failed to decode arguments: {error}"))
                })
            }
        }
    }
}

impl From<Vec<serde_json::Value>> for Arguments {
    fn from(values: Vec<serde_json::Value>) -> Self {
        Self(Arc::new(Repr::Json(values)))
    }
}

/// Compares the arguments as JSON values.
impl PartialEq for Arguments {
    fn eq(&self, other: &Self) -> bool {
        match (self.json_values(), other.json_values()) {
            (Ok(left), Ok(right)) => left == right,
            _ => false,
        }
    }
}

/// Shows the arguments as JSON values.
impl fmt::Debug for Arguments {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.json_values() {
            Ok(values) => values.fmt(formatter),
            Err(error) => write!(formatter, "<invalid arguments: {error}>"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn json_arguments_deserialize_without_conversion() {
        let arguments = Arguments::from(vec![
            serde_json::json!("name"),
            serde_json::json!({ "10": 1.5 }),
        ]);

        let (name, map): (String, HashMap<i64, f64>) = arguments.deserialize().unwrap();
        assert_eq!(name, "name");
        assert_eq!(map[&10], 1.5);
    }

    #[test]
    fn wrong_shape_is_invalid_arguments() {
        let arguments = Arguments::from(vec![serde_json::json!("name")]);

        let error = arguments.deserialize::<(i64,)>().unwrap_err();
        assert!(matches!(error, Error::InvalidArguments(_)), "{error}");
    }
}

#[cfg(test)]
#[cfg(feature = "messagepack")]
mod messagepack_tests {
    use std::collections::{BTreeMap, HashMap};

    use rmpv::Value;
    use serde::Deserialize;

    use super::*;

    fn messagepack(arguments: Vec<Value>) -> Arguments {
        let mut bytes = Vec::new();
        rmpv::encode::write_value(&mut bytes, &Value::Array(arguments)).unwrap();
        Arguments::from_messagepack(bytes).unwrap()
    }

    fn int_keyed_map() -> Value {
        Value::Map(vec![
            (Value::from(10), Value::from("ten")),
            (Value::from(20), Value::from("twenty")),
        ])
    }

    /// Values of different types, as in a map of dynamic parameters.
    fn params() -> Value {
        Value::Map(vec![
            (Value::from("int"), Value::from(100)),
            (Value::from("float"), Value::from(700.5)),
            (Value::from("bool"), Value::from(true)),
            (Value::from("text"), Value::from("x")),
            (Value::from("nested"), int_keyed_map()),
        ])
    }

    #[test]
    fn integer_map_keys_stay_integers() {
        let arguments = messagepack(vec![int_keyed_map()]);

        let (map,): (HashMap<i64, String>,) = arguments.deserialize().unwrap();
        assert_eq!(map[&10], "ten");
        let (map,): (BTreeMap<i64, String>,) = arguments.deserialize().unwrap();
        assert_eq!(map.keys().copied().collect::<Vec<_>>(), [10, 20]);
    }

    #[derive(Debug, PartialEq, Deserialize)]
    #[serde(untagged)]
    enum Param {
        Int(i64),
        Float(f64),
        Bool(bool),
        Text(String),
        Other(Value),
    }

    #[derive(Deserialize)]
    struct Market {
        id: i64,
        params: HashMap<String, Param>,
    }

    #[test]
    fn mixed_value_map_into_untagged_enum() {
        let market = Value::Map(vec![
            (Value::from("id"), Value::from(7)),
            (Value::from("params"), params()),
        ]);

        let (market,): (Market,) = messagepack(vec![market]).deserialize().unwrap();

        assert_eq!(market.id, 7);
        assert_eq!(market.params["int"], Param::Int(100));
        assert_eq!(market.params["float"], Param::Float(700.5));
        assert_eq!(market.params["bool"], Param::Bool(true));
        assert_eq!(market.params["text"], Param::Text("x".into()));
        assert_eq!(market.params["nested"], Param::Other(int_keyed_map()));
    }

    #[test]
    fn mixed_value_map_into_rmpv_value() {
        let (map,): (HashMap<String, Value>,) = messagepack(vec![params()]).deserialize().unwrap();

        assert_eq!(map["float"], Value::from(700.5));
        assert_eq!(map["nested"], int_keyed_map());
    }

    #[test]
    fn serde_json_value_rejects_integer_keyed_map() {
        let arguments = messagepack(vec![params()]);

        let error = arguments
            .deserialize::<(HashMap<String, serde_json::Value>,)>()
            .unwrap_err();
        assert!(
            error.to_string().contains("expected a string key"),
            "{error}"
        );
    }

    #[test]
    fn messagepack_arguments_convert_to_json() {
        let arguments = messagepack(vec![int_keyed_map()]);

        assert_eq!(
            arguments.to_json().unwrap(),
            vec![serde_json::json!({ "10": "ten", "20": "twenty" })]
        );
    }

    #[test]
    fn from_messagepack_rejects_non_array() {
        let mut bytes = Vec::new();
        rmpv::encode::write_value(&mut bytes, &Value::from("x")).unwrap();

        assert!(Arguments::from_messagepack(bytes).is_err());
    }
}
