//! Minimal DAP (Debug Adapter Protocol) wire format.
//!
//! Both request and response streams are framed as `Content-Length: N\r\n\r\n<json>`.

use std::io::{BufRead, Write};

use anyhow::{Result, bail};
use serde_json::Value;

pub fn write_message(writer: &mut impl Write, payload: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(payload)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()
}

pub fn read_message(reader: &mut impl BufRead) -> std::io::Result<Value> {
    let mut content_length = None;
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "debug adapter closed the stream",
            ));
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            match content_length {
                Some(length) => {
                    let mut buffer = vec![0u8; length];
                    reader.read_exact(&mut buffer)?;
                    return serde_json::from_slice(&buffer).map_err(|error| {
                        std::io::Error::new(std::io::ErrorKind::InvalidData, error)
                    });
                }
                None => continue,
            }
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse::<usize>().ok();
            }
        }
    }
}

pub fn request(command: &str, sequence: u64, arguments: Value) -> Value {
    serde_json::json!({
        "seq": sequence,
        "type": "request",
        "command": command,
        "arguments": arguments,
    })
}

/// Block until the response for `sequence` arrives. Events are ignored so the
/// handshake can be done before the event pump starts.
pub fn wait_for_response(reader: &mut impl BufRead, sequence: u64) -> Result<Value> {
    loop {
        let message = read_message(reader)
            .map_err(|error| anyhow::anyhow!("读取调试适配器响应失败: {error}"))?;
        let message_type = message
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if message_type != "response" {
            continue;
        }
        let request_seq = message
            .get("request_seq")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        if request_seq != sequence {
            continue;
        }
        let success = message
            .get("success")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if !success {
            let reason = message
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("未知错误");
            let body_message = message
                .get("body")
                .and_then(|body| body.get("error"))
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str);
            bail!("{}", body_message.unwrap_or(reason));
        }
        return Ok(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    #[test]
    fn frame_roundtrip() {
        let mut bytes = Vec::new();
        write_message(&mut bytes, &serde_json::json!({"seq": 7, "type": "response"}))
            .unwrap();
        let mut reader = BufReader::new(&bytes[..]);
        let message = read_message(&mut reader).unwrap();
        assert_eq!(message["seq"], 7);
    }

    #[test]
    fn response_error_is_reported() {
        let body = serde_json::json!({
            "seq": 9,
            "type": "response",
            "request_seq": 9,
            "success": false,
            "message": "program not found"
        })
        .to_string();
        let framed = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let mut reader = BufReader::new(framed.as_bytes());
        let error = wait_for_response(&mut reader, 9).unwrap_err();
        assert!(error.to_string().contains("program not found"));
    }

    #[test]
    fn wait_skips_events_until_response_arrives() {
        let event = r#"{"seq": 3, "type": "event", "event": "initialized"}"#;
        let response = r#"{"seq": 4, "type": "response", "request_seq": 1, "success": true}"#;
        let framed = format!(
            "Content-Length: {}\r\n\r\n{}Content-Length: {}\r\n\r\n{}",
            event.len(),
            event,
            response.len(),
            response
        );
        let mut reader = BufReader::new(framed.as_bytes());
        let message = wait_for_response(&mut reader, 1).unwrap();
        assert_eq!(message["success"], true);
    }
}
