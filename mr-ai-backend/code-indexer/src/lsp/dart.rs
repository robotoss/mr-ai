//! Dart LSP provider using stdio transport to dart language-server.

use crate::errors::{Error, Result};
use crate::lsp::interface::LspProvider;
use crate::types::CodeChunk;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use walkdir::WalkDir;

pub struct DartLsp;

impl LspProvider for DartLsp {
    fn enrich(root: &Path, _chunks: &mut [CodeChunk]) -> Result<()> {
        let mut proc = LspProcess::start()?;
        let init = format!(
            r#"{{
          "jsonrpc":"2.0","id":1,"method":"initialize",
          "params":{{"processId":null,"rootUri":"file://{root}",
                     "capabilities":{{"workspace":{{"configuration":true}}}},
                     "initializationOptions":{{"outline":true,"flutterOutline":true}}}}
        }}"#,
            root = root.to_string_lossy()
        );
        proc.send(&init)?;
        let _ = proc.recv()?;
        proc.send(r#"{"jsonrpc":"2.0","method":"initialized","params":{}}"#)?;

        let files = collect_dart_files(root);
        for p in files {
            let uri = format!("file://{}", p.to_string_lossy());
            let text = std::fs::read_to_string(&p).map_err(Error::from)?;
            let did_open = format!(
                r#"{{
              "jsonrpc":"2.0","method":"textDocument/didOpen","params":{{
                "textDocument":{{"uri":"{u}","languageId":"dart","version":1,"text":{t}}}
              }}
            }}"#,
                u = uri,
                t = serde_json::to_string(&text)?
            );
            proc.send(&did_open)?;
        }
        // TODO parse notifications and merge into chunks

        // Properly terminate the server process:
        proc.shutdown()?;
        Ok(())
    }
}

/// Return all `.dart` files under `root`, excluding .git, build, and .dart_tool folders.
fn collect_dart_files(root: &Path) -> Vec<PathBuf> {
    fn is_excluded_dir(p: &Path) -> bool {
        // OS-agnostic component check (".git", "build", ".dart_tool")
        p.components().any(|c| {
            let Some(s) = c.as_os_str().to_str() else {
                return false;
            };
            s == ".git" || s == "build" || s == ".dart_tool"
        })
    }

    let mut out = Vec::new();

    for entry in WalkDir::new(root).into_iter() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue, // skip unreadable entries
        };
        if !entry.file_type().is_file() {
            continue;
        }

        let p = entry.path();
        if is_excluded_dir(p) {
            continue;
        }

        // Case-sensitive check is fine for Dart; if нужно — сделай to_ascii_lowercase()
        if p.extension().and_then(|s| s.to_str()) == Some("dart") {
            out.push(p.to_path_buf());
        }
    }

    out
}

struct LspProcess {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: std::process::ChildStdout,
}

impl LspProcess {
    fn start() -> Result<Self> {
        let mut child = Command::new("dart")
            .arg("language-server")
            .arg("--client-id")
            .arg("code-indexer")
            .arg("--client-version")
            .arg("0.2")
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
        })
    }
    fn send(&mut self, json: &str) -> Result<()> {
        let header = format!("Content-Length: {}\r\n\r\n", json.as_bytes().len());
        self.stdin
            .write_all(header.as_bytes())
            .map_err(Error::from)?;
        self.stdin.write_all(json.as_bytes()).map_err(Error::from)?;
        self.stdin.flush().map_err(Error::from)
    }
    fn recv(&mut self) -> Result<String> {
        let mut header = String::new();
        let mut byte = [0u8; 1];
        let mut last4 = [0u8; 4];
        loop {
            self.stdout.read_exact(&mut byte).map_err(Error::from)?;
            header.push(byte[0] as char);
            last4.rotate_left(1);
            last4[3] = byte[0];
            if &last4 == b"\r\n\r\n" {
                break;
            }
        }
        let mut content_len = 0usize;
        for line in header.split("\r\n") {
            if let Some(v) = line.strip_prefix("Content-Length: ") {
                content_len = v.trim().parse().unwrap_or(0);
            }
        }
        let mut body = vec![0u8; content_len];
        self.stdout.read_exact(&mut body).map_err(Error::from)?;
        String::from_utf8(body).map_err(Error::from)
    }

    /// Gracefully shutdowns the LSP: send "shutdown", then "exit", and wait for process to terminate.
    fn shutdown(mut self) -> Result<()> {
        // LSP "shutdown" is a request with id. Some servers don't strictly reply; we still try once.
        let shutdown = r#"{"jsonrpc":"2.0","id":2,"method":"shutdown","params":null}"#;
        // Ignore errors on send/recv here to avoid masking earlier success.
        let _ = self.send(shutdown);
        let _ = self.recv(); // best-effort read a response

        // LSP "exit" is a notification.
        let exit = r#"{"jsonrpc":"2.0","method":"exit","params":null}"#;
        let _ = self.send(exit);

        // Wait for the process to exit.
        let _ = self
            .child
            .wait()
            .map_err(|_| crate::errors::Error::Spawn("wait failed"))?;
        Ok(())
    }
}

// As a safety net: if `shutdown()` wasn't called, try to kill the child on drop.
impl Drop for LspProcess {
    fn drop(&mut self) {
        // If the child is still running, try to terminate it. Ignore errors.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
