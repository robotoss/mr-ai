//! Minimal JSON-RPC stdio client for Dart Analysis Server (Dart ≥ 3.8.1).
//!
//! This client logs every request/response and transparently yields both
//! responses (with matching `id`) and server notifications (e.g. diagnostics).
//! We keep it blocking and simple; higher-level code is responsible for
//! deciding when to stop reading.

use crate::errors::{Error, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;
use tracing::debug;

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum RpcMessage {
    Response {
        id: Value,
        #[serde(default)]
        result: Option<Value>,
        #[serde(default)]
        error: Option<Value>,
    },
    Notification {
        method: String,
        #[serde(default)]
        params: Value,
    },
}

pub struct LspProcess {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
    next_id: u64,
}

impl LspProcess {
    pub fn start() -> Result<Self> {
        let mut child = Command::new("dart")
            .arg("language-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|_| Error::Spawn("failed to start dart language-server"))?;

        let stdin = child.stdin.take().ok_or(Error::Spawn("no stdin"))?;
        let stdout = child.stdout.take().ok_or(Error::Spawn("no stdout"))?;
        Ok(Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        })
    }

    /// Returns next JSON-RPC id as a JSON value.
    pub fn next_id(&mut self) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        Value::from(id)
    }

    /// Sends a JSON-RPC message with `Content-Length` header.
    pub fn send(&mut self, json: &Value) -> Result<()> {
        let body = serde_json::to_vec(json)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.stdin.write_all(header.as_bytes())?;
        self.stdin.write_all(&body)?;
        self.stdin.flush()?;
        debug!("LSP → {}", serde_json::to_string(json).unwrap_or_default());
        Ok(())
    }

    /// Receives a single message (response or notification).
    /// Blocks until a complete LSP frame is read.
    pub fn recv(&mut self) -> Result<RpcMessage> {
        // Read header until CRLFCRLF
        let mut header = Vec::<u8>::new();
        let mut last4 = [0u8; 4];
        let mut b = [0u8; 1];
        loop {
            self.stdout.read_exact(&mut b)?;
            header.push(b[0]);
            last4.rotate_left(1);
            last4[3] = b[0];
            if &last4 == b"\r\n\r\n" {
                break;
            }
            if header.len() > 8192 {
                return Err(Error::LspProtocol("header too large"));
            }
        }

        // Parse Content-Length
        let s = String::from_utf8(header).map_err(Error::from)?;
        let mut content_len = 0usize;
        for line in s.split("\r\n") {
            if let Some(v) = line.strip_prefix("Content-Length: ") {
                content_len = v.trim().parse().unwrap_or(0);
            }
        }
        if content_len == 0 {
            return Err(Error::LspProtocol("missing content length"));
        }

        // Read body
        let mut body = vec![0u8; content_len];
        self.stdout.read_exact(&mut body)?;
        debug!("LSP ← {}", String::from_utf8_lossy(&body));
        let msg: RpcMessage = serde_json::from_slice(&body)?;
        Ok(msg)
    }

    /// Best-effort graceful shutdown.
    pub fn shutdown(&mut self) -> Result<()> {
        let id = self.next_id();
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":"shutdown"}))?;
        let deadline = std::time::Instant::now() + Duration::from_millis(400);
        while std::time::Instant::now() < deadline {
            if let Ok(msg) = self.recv() {
                if let RpcMessage::Response { id: rid, .. } = msg {
                    if rid == id {
                        break;
                    }
                }
            } else {
                break;
            }
        }
        self.send(&json!({"jsonrpc":"2.0","method":"exit","params":{}}))?;
        let _ = self.child.wait();
        Ok(())
    }
}

impl Drop for LspProcess {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}
