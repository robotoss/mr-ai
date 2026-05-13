//! Rust client for the Dart Analyzer sidecar.
//!
//! Speaks the same Content-Length-framed JSON-RPC dialect as the Dart
//! Analysis Server (see `client.rs`), so the framing logic here is a
//! lighter twin: only the methods we actually call ship.
//!
//! The client is intentionally synchronous internally (subprocess + stdio
//! is sync) and exposed through small `&mut self` methods. Callers that
//! need async friendly behaviour wrap the whole client in
//! `tokio::task::spawn_blocking` — the same pattern the rest of the
//! indexer uses.
//!
//! The sidecar binary is selected via:
//! - `DART_SIDECAR_BINARY` — an executable path (run directly), or
//! - `DART_SIDECAR_DART_ENTRYPOINT` — a `.dart` source file run with
//!   `dart run`.
//!
//! When neither is set, [`SidecarClient::start`] returns `Disabled` and
//! the analyzer falls back to its tree-sitter path.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use tracing::{debug, warn};

#[derive(Debug, Error)]
pub enum SidecarError {
    #[error("sidecar disabled (no DART_SIDECAR_BINARY / DART_SIDECAR_DART_ENTRYPOINT)")]
    Disabled,
    #[error("failed to spawn sidecar: {0}")]
    Spawn(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("sidecar returned error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("malformed sidecar response: {0}")]
    Protocol(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct InitializeParams {
    pub workspace: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dart_sdk: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct InitializeResult {
    #[serde(default)]
    pub analyzer_version: String,
    #[serde(default)]
    pub dart_version: String,
    #[serde(default)]
    pub supports: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExtractEdgesParams {
    pub files: Vec<String>,
    pub kinds: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SidecarEdge {
    pub from_fqn: String,
    pub to_fqn: String,
    pub edge_type: String,
    #[serde(default = "default_weight")]
    pub weight: f32,
    #[serde(default)]
    pub meta: Option<Value>,
}

fn default_weight() -> f32 {
    1.0
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExtractEdgesResult {
    #[serde(default)]
    pub edges: Vec<SidecarEdge>,
    #[serde(default)]
    pub coverage: std::collections::BTreeMap<String, u64>,
}

pub struct SidecarClient {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    next_id: u64,
}

impl SidecarClient {
    /// Try to start a sidecar from environment-driven configuration.
    /// Returns `Err(Disabled)` when no entrypoint is configured so callers
    /// can transparently degrade to the tree-sitter-only path.
    pub fn start() -> Result<Self, SidecarError> {
        let cmd = Self::build_command()?;
        Self::spawn(cmd)
    }

    fn build_command() -> Result<Command, SidecarError> {
        if let Ok(path) = std::env::var("DART_SIDECAR_BINARY") {
            if !path.trim().is_empty() {
                return Ok(Command::new(path));
            }
        }
        if let Ok(entry) = std::env::var("DART_SIDECAR_DART_ENTRYPOINT") {
            if !entry.trim().is_empty() {
                let mut cmd = Command::new("dart");
                cmd.arg("run").arg(entry);
                return Ok(cmd);
            }
        }
        Err(SidecarError::Disabled)
    }

    fn spawn(mut cmd: Command) -> Result<Self, SidecarError> {
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| SidecarError::Spawn(e.to_string()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| SidecarError::Spawn("missing stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SidecarError::Spawn("missing stdout".into()))?;
        Ok(Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        })
    }

    /// Send a request and block waiting for the matching response.
    fn call<P: Serialize, R: for<'de> Deserialize<'de>>(
        &mut self,
        method: &str,
        params: &P,
    ) -> Result<R, SidecarError> {
        let id = self.next_id;
        self.next_id += 1;
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let bytes = serde_json::to_vec(&body)?;
        let header = format!("Content-Length: {}\r\n\r\n", bytes.len());
        self.stdin.write_all(header.as_bytes())?;
        self.stdin.write_all(&bytes)?;
        self.stdin.flush()?;
        debug!(target = "sidecar", method, id, "→ request");
        let response = self.read_message()?;

        if let Some(err) = response.get("error") {
            let code = err.get("code").and_then(Value::as_i64).unwrap_or(-1);
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            return Err(SidecarError::Rpc { code, message });
        }
        let result = response
            .get("result")
            .cloned()
            .unwrap_or(Value::Null);
        let parsed: R = serde_json::from_value(result)
            .map_err(|e| SidecarError::Protocol(e.to_string()))?;
        Ok(parsed)
    }

    /// Read a single Content-Length-framed JSON-RPC message.
    fn read_message(&mut self) -> Result<Value, SidecarError> {
        let mut header = Vec::<u8>::new();
        let mut sliding = [0u8; 4];
        loop {
            let mut byte = [0u8; 1];
            let n = self.stdout.read(&mut byte)?;
            if n == 0 {
                return Err(SidecarError::Protocol("eof".into()));
            }
            header.push(byte[0]);
            sliding.copy_within(1..4, 0);
            sliding[3] = byte[0];
            if sliding == *b"\r\n\r\n" {
                break;
            }
            if header.len() > 8192 {
                return Err(SidecarError::Protocol("header too long".into()));
            }
        }
        let header = String::from_utf8(header)
            .map_err(|_| SidecarError::Protocol("non-utf8 header".into()))?;
        let mut length: usize = 0;
        for line in header.lines() {
            if let Some(rest) = line.strip_prefix("Content-Length: ") {
                length = rest
                    .trim()
                    .parse()
                    .map_err(|_| SidecarError::Protocol("bad content length".into()))?;
            }
        }
        if length == 0 {
            return Err(SidecarError::Protocol("missing Content-Length".into()));
        }
        let mut body = vec![0u8; length];
        self.stdout.read_exact(&mut body)?;
        let value: Value = serde_json::from_slice(&body)?;
        Ok(value)
    }

    pub fn initialize(
        &mut self,
        workspace: impl AsRef<Path>,
    ) -> Result<InitializeResult, SidecarError> {
        let params = InitializeParams {
            workspace: workspace.as_ref().to_string_lossy().into_owned(),
            dart_sdk: std::env::var("DART_SDK").ok(),
        };
        self.call("initialize", &params)
    }

    pub fn extract_edges(
        &mut self,
        files: Vec<String>,
        kinds: Vec<String>,
    ) -> Result<ExtractEdgesResult, SidecarError> {
        if files.is_empty() {
            return Ok(ExtractEdgesResult::default());
        }
        let params = ExtractEdgesParams { files, kinds };
        self.call("extractEdges", &params)
    }

    pub fn shutdown(mut self) -> Result<(), SidecarError> {
        let _: Value = self.call("shutdown", &json!({}))?;
        // Give the child a moment to exit cleanly; kill if it's still running.
        std::thread::sleep(Duration::from_millis(200));
        if self.child.try_wait()?.is_none() {
            warn!(target = "sidecar", "sidecar did not exit; killing");
            let _ = self.child.kill();
        }
        Ok(())
    }
}

impl Drop for SidecarClient {
    fn drop(&mut self) {
        // Best-effort cleanup on accidental drop.
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_returns_disabled_when_env_unset() {
        // Ensure neither var is set in this thread.
        unsafe {
            std::env::remove_var("DART_SIDECAR_BINARY");
            std::env::remove_var("DART_SIDECAR_DART_ENTRYPOINT");
        }
        let result = SidecarClient::start();
        assert!(matches!(result, Err(SidecarError::Disabled)));
    }

    #[test]
    fn extract_edges_default_value_round_trip() {
        let raw = serde_json::json!({"edges": [], "coverage": {}});
        let parsed: ExtractEdgesResult = serde_json::from_value(raw).unwrap();
        assert!(parsed.edges.is_empty());
        assert!(parsed.coverage.is_empty());
    }

    #[test]
    fn sidecar_edge_default_weight() {
        let raw = serde_json::json!({
            "from_fqn": "a",
            "to_fqn": "b",
            "edge_type": "data_flow"
        });
        let parsed: SidecarEdge = serde_json::from_value(raw).unwrap();
        assert!((parsed.weight - 1.0).abs() < 1e-6);
        assert!(parsed.meta.is_none());
    }
}
