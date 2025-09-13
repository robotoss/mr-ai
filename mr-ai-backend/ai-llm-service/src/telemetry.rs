use std::io::{self, IsTerminal};
use std::str::FromStr;

use tracing::Level;
use tracing_subscriber::filter::Directive;
use tracing_subscriber::fmt::format::Writer;
use tracing_subscriber::fmt::time::FormatTime;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{EnvFilter, Layer, filter, fmt};

/// Crate target prefix used to filter only library-originated logs.
pub const TARGET_PREFIX: &str = "ai_llm_service";

/// RFC3339 UTC timer implemented via `chrono` (no extra features).
/// Example output: `2025-09-12T10:20:30Z`
#[derive(Clone, Debug, Default)]
struct ChronoRfc3339Utc;

impl FormatTime for ChronoRfc3339Utc {
    fn format_time(&self, w: &mut Writer<'_>) -> std::fmt::Result {
        let now = chrono::Utc::now();
        // Keep timestamps compact: no fractional seconds, Z-suffix
        let s = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        w.write_str(&s)
    }
}

/// Build a **library-scoped** formatting layer that renders ONLY events emitted by this crate.
///
/// - RFC3339 UTC timestamps
/// - Compact single-line format
/// - `file:line` and target (module path)
/// - Span close events (duration at the end of spans)
/// - ANSI colors only when stdout is a terminal
///
/// This layer uses a per-event filter so it does **not** affect logs from other crates.
/// Compose it in the binary together with your global subscriber.
pub fn layer<S>() -> impl Layer<S> + Send + Sync
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    let use_ansi = io::stdout().is_terminal();

    // Accept only events whose target starts with our crate prefix.
    let only_this_crate = filter::filter_fn(|meta| meta.target().starts_with(TARGET_PREFIX));

    fmt::layer()
        .with_timer(ChronoRfc3339Utc::default())
        .with_level(true) // show level
        .with_target(true) // show module path (target)
        .with_file(true)
        .with_line_number(true)
        .with_ansi(use_ansi)
        // Log span close to get durations for instrumented functions
        .with_span_events(fmt::format::FmtSpan::CLOSE)
        .event_format(
            fmt::format()
                .compact() // single-line, tidy output
                .with_source_location(true),
        )
        .with_filter(only_this_crate)
}

/// Helper to build a level directive for **this** library only.
/// Example:
/// `EnvFilter::new("info").add_directive(level_directive(Level::DEBUG))`
pub fn level_directive(level: Level) -> Directive {
    // Format like `ai_llm_service=debug`
    let s = format!("{TARGET_PREFIX}={}", level.as_str().to_lowercase());
    Directive::from_str(&s).expect("valid level directive")
}

/// Convenience: create an EnvFilter from env or fallback default,
/// then apply a per-crate level directive for this library.
///
/// Example fallback: `default = "info"`, `level = Level::DEBUG`
/// resulting filter displays all logs at INFO globally,
/// and DEBUG for ai-llm-service only.
pub fn env_filter_with_level(default: &str, level: Level) -> EnvFilter {
    let base = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    base.add_directive(level_directive(level))
}
