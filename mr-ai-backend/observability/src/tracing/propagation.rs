//! W3C `traceparent` propagation across the worker job boundary.
//!
//! Webhook handler → job payload → worker `process_one` is the only
//! seam where the current span context would otherwise be lost. Both
//! helpers are safe to call when OTLP is disabled: the global
//! propagator falls back to a noop and the helpers leave the payload
//! untouched / span unparented.

use std::collections::HashMap;

use opentelemetry::propagation::{Extractor, Injector};
use serde_json::Value;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;

/// JSON key in the job payload that carries the W3C traceparent header.
pub const TRACEPARENT_KEY: &str = "traceparent";

/// Carrier impl over a `HashMap` so the upstream propagator can read
/// from / write into a plain key-value bag we then serialize into JSON.
struct MapCarrier<'a>(&'a mut HashMap<String, String>);

impl<'a> Injector for MapCarrier<'a> {
    fn set(&mut self, key: &str, value: String) {
        self.0.insert(key.to_string(), value);
    }
}

impl<'a> Extractor for MapCarrier<'a> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(|s| s.as_str())
    }
    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|s| s.as_str()).collect()
    }
}

/// Write the current span's traceparent into `payload[TRACEPARENT_KEY]`.
///
/// When the global propagator is the noop default (e.g. OTLP disabled),
/// no key is added — the function silently no-ops. Use this on the
/// webhook side before `jobs::enqueue`.
pub fn inject_into_payload(payload: &mut Value) {
    let mut carrier_map: HashMap<String, String> = HashMap::new();
    let cx = Span::current().context();
    opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.inject_context(&cx, &mut MapCarrier(&mut carrier_map));
    });
    if let Some(tp) = carrier_map.remove(TRACEPARENT_KEY) {
        if let Some(obj) = payload.as_object_mut() {
            obj.insert(TRACEPARENT_KEY.to_string(), Value::String(tp));
        }
    }
}

/// Read `payload[TRACEPARENT_KEY]` and attach the parsed context as the
/// parent of the current span. No-op when the field is absent or not a
/// string. Use this on the worker side at the top of `process_one`.
pub fn set_parent_from_payload(payload: &Value) {
    let Some(tp) = payload
        .get(TRACEPARENT_KEY)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    else {
        return;
    };
    let mut carrier_map: HashMap<String, String> = HashMap::new();
    carrier_map.insert(TRACEPARENT_KEY.to_string(), tp.to_string());
    let cx = opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.extract(&MapCarrier(&mut carrier_map))
    });
    // `set_parent` returns Result in tracing-opentelemetry 0.32+;
    // failure here only means the current dispatcher isn't an OTel
    // bridge (e.g. OTLP disabled), which is the same no-op semantic
    // we want.
    let _ = Span::current().set_parent(cx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState};
    use opentelemetry::Context;
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use serde_json::json;

    /// Inject a known span context into a JSON payload and confirm the
    /// resulting `traceparent` round-trips back through the extractor
    /// into a context whose trace_id matches the original.
    #[test]
    fn traceparent_round_trip_through_payload() {
        // Sprint 2 always installs the W3C propagator inside
        // `install_otlp_layer`; in tests we set it explicitly so the
        // assertion doesn't depend on prior init order.
        opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());

        let trace_id = TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap();
        let span_id = SpanId::from_hex("00f067aa0ba902b7").unwrap();
        let span_ctx = SpanContext::new(
            trace_id,
            span_id,
            TraceFlags::SAMPLED,
            true,
            TraceState::default(),
        );
        let parent = Context::current().with_remote_span_context(span_ctx);

        // Inject using the parent context directly (don't depend on
        // a running tracing span).
        let mut carrier: HashMap<String, String> = HashMap::new();
        opentelemetry::global::get_text_map_propagator(|p| {
            p.inject_context(&parent, &mut MapCarrier(&mut carrier));
        });
        let traceparent = carrier
            .remove(TRACEPARENT_KEY)
            .expect("propagator should produce traceparent");

        let mut payload = json!({"event_id": "abc-123"});
        payload
            .as_object_mut()
            .unwrap()
            .insert(TRACEPARENT_KEY.to_string(), Value::String(traceparent));

        // Extract back into a context.
        let tp_str = payload.get(TRACEPARENT_KEY).unwrap().as_str().unwrap();
        let mut carrier_back: HashMap<String, String> = HashMap::new();
        carrier_back.insert(TRACEPARENT_KEY.to_string(), tp_str.to_string());
        let extracted = opentelemetry::global::get_text_map_propagator(|p| {
            p.extract(&MapCarrier(&mut carrier_back))
        });
        let extracted_span = extracted.span().span_context().clone();
        assert_eq!(extracted_span.trace_id(), trace_id);
        assert_eq!(extracted_span.span_id(), span_id);
    }

    /// Payload without a `traceparent` field must not panic and must
    /// not leave any side effect on the current span.
    #[test]
    fn extract_returns_none_without_traceparent_field() {
        let payload = json!({"event_id": "abc-123"});
        // Must not panic.
        set_parent_from_payload(&payload);
    }

    #[test]
    fn inject_no_ops_when_payload_is_not_an_object() {
        let mut payload = Value::Array(vec![]);
        // Must not panic on non-object payloads; nothing inserted.
        inject_into_payload(&mut payload);
        assert!(payload.as_array().unwrap().is_empty());
    }
}
