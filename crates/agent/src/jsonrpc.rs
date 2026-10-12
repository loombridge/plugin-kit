//! JSON-RPC 2.0, the message format MCP runs on: requests, notifications,
//! responses, and errors.

use serde_json::{Map, Value, json};

/// The error codes this server sends.
pub mod code {
    /// The message isn't JSON.
    pub const PARSE_ERROR: i64 = -32_700;
    /// The JSON isn't a JSON-RPC request.
    pub const INVALID_REQUEST: i64 = -32_600;
    /// No method has that name.
    pub const METHOD_NOT_FOUND: i64 = -32_601;
    /// The method's parameters are wrong.
    pub const INVALID_PARAMS: i64 = -32_602;
    /// Something failed inside the server.
    pub const INTERNAL_ERROR: i64 = -32_603;
    /// The agent's approval was withdrawn, or the person refused it: the
    /// session is over.
    pub const NOT_ALLOWED: i64 = -32_001;
}

/// One JSON-RPC message, sorted by what it is.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    /// A request: it gets a response with the same ID.
    Request {
        /// The ID to answer with.
        id: Value,
        /// The method.
        method: String,
        /// Its parameters, an object (or `null` when there are none).
        params: Value,
    },
    /// A notification: it gets no response.
    Notification {
        /// The method.
        method: String,
        /// Its parameters.
        params: Value,
    },
    /// A response to a request the server sent. This server sends none, so
    /// these are ignored.
    Response,
}

impl Message {
    /// Sorts a JSON value into a message.
    ///
    /// # Errors
    ///
    /// The error response to send if it isn't a JSON-RPC 2.0 message.
    pub fn parse(value: Value) -> Result<Self, Value> {
        let Value::Object(mut object) = value else {
            let error = if value.is_array() {
                "batches aren't supported: send one message at a time"
            } else {
                "a message must be a JSON object"
            };
            return Err(error_response(Value::Null, code::INVALID_REQUEST, error));
        };
        let id = object.remove("id");
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(error_response(
                id.unwrap_or(Value::Null),
                code::INVALID_REQUEST,
                "`jsonrpc` must be \"2.0\"",
            ));
        }
        match (object.remove("method"), id) {
            (Some(Value::String(method)), Some(id)) if valid_id(&id) => Ok(Self::Request {
                id,
                method,
                params: object.remove("params").unwrap_or(Value::Null),
            }),
            (Some(Value::String(method)), None) => Ok(Self::Notification {
                method,
                params: object.remove("params").unwrap_or(Value::Null),
            }),
            (None, Some(_)) if object.contains_key("result") || object.contains_key("error") => {
                Ok(Self::Response)
            }
            (_, id) => Err(error_response(
                id.filter(valid_id).unwrap_or(Value::Null),
                code::INVALID_REQUEST,
                "a request needs a string `method`, and an `id` that's a string or a number",
            )),
        }
    }
}

fn valid_id(id: &Value) -> bool {
    id.is_string() || id.is_number()
}

/// A successful response.
#[must_use]
pub fn response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// An error response.
#[must_use]
pub fn error_response(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() },
    })
}

/// A request, as a client sends one.
#[must_use]
pub fn request(id: u64, method: &str, params: Value) -> Value {
    let mut object = Map::new();
    object.insert("jsonrpc".to_owned(), json!("2.0"));
    object.insert("id".to_owned(), json!(id));
    object.insert("method".to_owned(), json!(method));
    if !params.is_null() {
        object.insert("params".to_owned(), params);
    }
    Value::Object(object)
}

/// A notification, as a client sends one.
#[must_use]
pub fn notification(method: &str, params: Value) -> Value {
    let mut object = Map::new();
    object.insert("jsonrpc".to_owned(), json!("2.0"));
    object.insert("method".to_owned(), json!(method));
    if !params.is_null() {
        object.insert("params".to_owned(), params);
    }
    Value::Object(object)
}

/// Whether a JSON value is a request (as opposed to a notification or a
/// response), so a transport knows whether to wait for an answer.
#[must_use]
pub fn is_request(value: &Value) -> bool {
    value.get("method").is_some() && value.get("id").is_some_and(valid_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_sort_by_kind() {
        assert_eq!(
            Message::parse(json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})),
            Ok(Message::Request {
                id: json!(1),
                method: "ping".to_owned(),
                params: Value::Null
            })
        );
        assert_eq!(
            Message::parse(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})),
            Ok(Message::Notification {
                method: "notifications/initialized".to_owned(),
                params: Value::Null
            })
        );
        assert_eq!(
            Message::parse(json!({"jsonrpc": "2.0", "id": "a", "result": {}})),
            Ok(Message::Response)
        );
        for bad in [
            json!([]),
            json!("ping"),
            json!({"id": 1, "method": "ping"}),
            json!({"jsonrpc": "2.0", "id": 1, "method": 3}),
            json!({"jsonrpc": "2.0", "id": {"x": 1}, "method": "ping"}),
        ] {
            let error = Message::parse(bad.clone()).unwrap_err();
            assert_eq!(error["error"]["code"], code::INVALID_REQUEST, "{bad}");
        }
    }

    #[test]
    fn requests_and_notifications_are_well_formed() {
        assert!(is_request(&request(1, "ping", Value::Null)));
        assert!(!is_request(&notification(
            "notifications/initialized",
            Value::Null
        )));
        assert_eq!(
            error_response(json!(7), code::METHOD_NOT_FOUND, "nope")["error"]["code"],
            -32_601
        );
    }
}
