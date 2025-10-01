//! RouterAst selects language providers by file extension and never panics.

use super::{
    dart::DartAst, generic_text::GenericTextAst, interface::AstProvider, javascript::JavascriptAst,
    rust::RustAst, typescript::TypescriptAst,
};
use crate::errors::Result;
use crate::types::CodeChunk;
use std::{path::Path, time::Instant};
use tracing::{debug, error, info, warn};

pub struct RouterAst;

impl RouterAst {
    /// Parse a file by extension. On failure falls back to GenericTextAst.
    pub fn parse_file(path: &Path) -> Result<Vec<CodeChunk>> {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        let started = Instant::now();
        debug!(target: "router", file = %path.display(), %ext, "RouterAst: selecting provider");

        // primary parse by extension with per-branch logging
        let primary = match ext.as_str() {
            "dart" => {
                debug!(target: "router", file = %path.display(), "RouterAst: using DartAst");
                DartAst::parse_file(path)
            }
            "rs" => {
                debug!(target: "router", file = %path.display(), "RouterAst: using RustAst");
                RustAst::parse_file(path)
            }
            "js" | "jsx" => {
                debug!(target: "router", file = %path.display(), "RouterAst: using JavascriptAst");
                JavascriptAst::parse_file(path)
            }
            "ts" | "tsx" => {
                debug!(target: "router", file = %path.display(), "RouterAst: using TypescriptAst");
                TypescriptAst::parse_file(path)
            }
            // Known config and unknown but useful files go via GenericTextAst
            "yaml" | "yml" | "json" | "arb" | "xml" | "plist" | "toml" | "gradle"
            | "properties" | "kt" | "kts" | "swift" | "java" => {
                debug!(target: "router", file = %path.display(), %ext, "RouterAst: using GenericTextAst (known config)");
                GenericTextAst::parse_file(path)
            }
            _ => {
                debug!(target: "router", file = %path.display(), %ext, "RouterAst: using GenericTextAst (fallback by ext)");
                GenericTextAst::parse_file(path)
            }
        };

        match primary {
            Ok(chunks) => {
                info!(
                    target: "router",
                    file = %path.display(),
                    %ext,
                    len = chunks.len(),
                    elapsed_ms = started.elapsed().as_millis(),
                    "RouterAst: parsed by primary provider"
                );
                // small extra visibility for empty results
                if chunks.is_empty() {
                    warn!(target: "router", file = %path.display(), "RouterAst: parsed successfully but got 0 chunks");
                }
                Ok(chunks)
            }
            Err(e) => {
                warn!(
                    target: "router",
                    file = %path.display(),
                    %ext,
                    error = %e,
                    "RouterAst: primary provider failed, falling back to GenericTextAst"
                );
                let fb_started = Instant::now();
                match GenericTextAst::parse_file(path) {
                    Ok(fb_chunks) => {
                        info!(
                            target: "router",
                            file = %path.display(),
                            len = fb_chunks.len(),
                            elapsed_ms = fb_started.elapsed().as_millis(),
                            "RouterAst: fallback GenericTextAst parsed"
                        );
                        if fb_chunks.is_empty() {
                            warn!(target: "router", file = %path.display(), "RouterAst: fallback produced 0 chunks");
                        }
                        Ok(fb_chunks)
                    }
                    Err(e2) => {
                        error!(
                            target: "router",
                            file = %path.display(),
                            primary_error = %e,
                            fallback_error = %e2,
                            "RouterAst: both primary and fallback failed"
                        );
                        Err(e2)
                    }
                }
            }
        }
    }
}
