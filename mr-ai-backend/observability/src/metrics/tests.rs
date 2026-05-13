//! Tests for metric emission. Uses `metrics::with_local_recorder` so
//! tests don't fight over the global recorder and can run in parallel.

use metrics::{with_local_recorder, Unit};
use metrics_util::debugging::{DebugValue, DebuggingRecorder};

use crate::{counter, histogram};

/// Job-completion path: emit `jobs_done_total{kind, outcome}` once and
/// confirm it lands in the recorder snapshot with the expected labels.
#[test]
fn counter_increments_on_job_complete() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();

    with_local_recorder(&recorder, || {
        counter!(
            super::JOBS_DONE_TOTAL,
            "kind" => "Reindex",
            "outcome" => "ok",
        )
        .increment(1);
    });

    let snapshot = snapshotter.snapshot().into_vec();
    let found = snapshot.iter().find(|(key, _, _, _)| {
        key.key().name() == super::JOBS_DONE_TOTAL
            && key
                .key()
                .labels()
                .any(|l| l.key() == "outcome" && l.value() == "ok")
    });
    let (_, _, _, value) = found.expect("jobs_done_total{outcome=ok} not recorded");
    assert!(matches!(value, DebugValue::Counter(1)), "got {value:?}");
}

/// `/retrieve` latency path: record one histogram sample and confirm
/// the snapshot exposes it.
#[test]
fn histogram_records_retrieve_latency() {
    let recorder = DebuggingRecorder::new();
    let snapshotter = recorder.snapshotter();

    with_local_recorder(&recorder, || {
        histogram!(super::RETRIEVE_LATENCY_SECONDS).record(0.142_f64);
    });

    let snapshot = snapshotter.snapshot().into_vec();
    let found = snapshot
        .iter()
        .find(|(key, _, _, _)| key.key().name() == super::RETRIEVE_LATENCY_SECONDS);
    let (_, _, _, value) = found.expect("retrieve_latency_seconds not recorded");
    match value {
        DebugValue::Histogram(samples) => {
            assert_eq!(samples.len(), 1, "expected one sample");
            let sample: f64 = samples[0].into_inner();
            assert!((sample - 0.142).abs() < 1e-9, "got {sample}");
        }
        other => panic!("expected histogram, got {other:?}"),
    }
}

/// Sanity check that the canonical metric name constants are wired up
/// and free of typos — guards against silent rename drift.
#[test]
fn metric_names_are_stable() {
    assert_eq!(super::JOBS_ENQUEUED_TOTAL, "jobs_enqueued_total");
    assert_eq!(super::JOBS_DONE_TOTAL, "jobs_done_total");
    assert_eq!(super::JOB_DURATION_SECONDS, "job_duration_seconds");
    assert_eq!(super::LLM_CALLS_TOTAL, "llm_calls_total");
    assert_eq!(super::LLM_COST_MICRO_USD_TOTAL, "llm_cost_micro_usd_total");
    assert_eq!(super::LLM_LATENCY_SECONDS, "llm_latency_seconds");
    assert_eq!(super::RETRIEVE_LATENCY_SECONDS, "retrieve_latency_seconds");
    assert_eq!(super::RETRIEVE_HITS, "retrieve_hits");
    assert_eq!(super::QDRANT_SEARCH_LATENCY_SECONDS, "qdrant_search_latency_seconds");
    assert_eq!(super::WEBHOOK_RECEIVED_TOTAL, "webhook_received_total");
    assert_eq!(super::MR_REVIEWS_TOTAL, "mr_reviews_total");
    // touch unused import so clippy doesn't complain about Unit in scope
    let _: Option<Unit> = None;
}
