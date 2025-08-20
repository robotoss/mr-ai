//! Lightweight progress reporting for RAG pipeline.
//!
//! Use `NoopProgress` for servers (default) and `IndicatifProgress` (feature
//! "progress") for CLI/TTY.

use indicatif::{ProgressBar, ProgressStyle};

/// Minimal progress interface used inside the ask() pipeline.
pub trait Progress: Send + Sync {
    /// Set known total steps (optional).
    fn set_total(&self, _n: u64) {}
    /// Advance by one step and show a short message.
    fn step(&self, _msg: &str) {}
    /// Replace current message without advancing.
    fn message(&self, _msg: &str) {}
    /// Finish the UI.
    fn finish(&self, _msg: &str) {}
}

/// No-op reporter for servers/headless runs.
#[derive(Default, Clone, Copy)]
pub struct NoopProgress;
impl Progress for NoopProgress {}

/// Indicatif-based spinner/bar (enabled with `--features progress`).
pub struct IndicatifProgress {
    pb: ProgressBar,
}

impl IndicatifProgress {
    /// Spinner (unknown total).
    pub fn spinner() -> Self {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::with_template("{spinner} {msg}")
                .unwrap()
                .tick_chars("-\\|/ "),
        );
        pb.enable_steady_tick(std::time::Duration::from_millis(80));
        Self { pb }
    }

    /// Bounded bar (known total).
    pub fn bar(len: u64) -> Self {
        let pb = ProgressBar::new(len);
        pb.set_style(
            ProgressStyle::with_template("{bar:40.cyan/blue} {pos:>3}/{len:3} {msg}").unwrap(),
        );
        Self { pb }
    }
}

impl Progress for IndicatifProgress {
    fn set_total(&self, n: u64) {
        self.pb.set_length(n);
    }
    fn step(&self, msg: &str) {
        self.pb.inc(1);
        self.pb.set_message(msg.to_string());
    }
    fn message(&self, msg: &str) {
        self.pb.set_message(msg.to_string());
    }
    fn finish(&self, msg: &str) {
        self.pb.finish_with_message(msg.to_string());
    }
}
