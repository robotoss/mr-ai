//! Builders converting high-level `RagFilter` into Qdrant filters.
//!
//! qdrant-client 1.15 specifics:
//! - `Condition` is a wrapper with `condition_one_of: Option<condition::ConditionOneOf>`.
//!   There are no helper constructors like `Condition::Field`; you must set the enum.
//! - `FieldCondition.r#match` expects a `Match`, which wraps `r#match::MatchValue`.
//! - Floats are not supported by `MatchValue`; use `Range { gte, lte }` for equality-like behavior.

use crate::record::RagFilter;
use qdrant_client::qdrant::{
    Condition, FieldCondition, Filter, Match, Range, condition, r#match::MatchValue,
};
use serde_json::Value as J;
use tracing::trace;

/// Converts a high-level `RagFilter` into a concrete Qdrant `Filter`.
///
/// Supported mappings:
/// - `BySource("...")` -> exact equality via `MatchValue::Keyword`
/// - `ByFieldEq { key, value }`:
///   - string  -> `MatchValue::Keyword`
///   - integer -> `MatchValue::Integer`
///   - boolean -> `MatchValue::Boolean`
///   - float   -> `Range { gte = val, lte = val }`
/// - `And([...])` -> flatten into `must`
/// - `Or([...])`  -> each sub-filter wrapped into `Condition::Filter` and appended to `should`
pub fn to_qdrant_filter(f: &RagFilter) -> Filter {
    trace!("filters::to_qdrant_filter kind={}", kind_of_filter(f));
    match f {
        RagFilter::BySource(src) => Filter {
            must: vec![condition_field_eq("source", &J::String(src.clone()))],
            ..Default::default()
        },

        RagFilter::ByFieldEq { key, value } => Filter {
            must: vec![condition_field_eq(key, value)],
            ..Default::default()
        },

        RagFilter::And(list) => {
            let mut out = Filter::default();
            for sub in list {
                let sf = to_qdrant_filter(sub);
                out.must.extend(sf.must);
                out.should.extend(sf.should);
                out.must_not.extend(sf.must_not);
            }
            out
        }

        RagFilter::Or(list) => {
            let mut out = Filter::default();
            for sub in list {
                let sf = to_qdrant_filter(sub);
                // Wrap sub-filter into a nested filter condition.
                out.should.push(Condition {
                    condition_one_of: Some(condition::ConditionOneOf::Filter(sf)),
                });
            }
            out
        }
    }
}

/// Builds a single equality-like `Condition` for a field.
///
/// For floats we express equality as a narrow range: `gte == lte == value`.
fn condition_field_eq(key: impl Into<String>, value: &J) -> Condition {
    let key = key.into();

    // Build FieldCondition with either r#match or range set.
    let field = match value {
        J::String(s) => FieldCondition {
            key,
            r#match: Some(Match {
                match_value: Some(MatchValue::Keyword(s.clone())),
            }),
            ..Default::default()
        },

        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                FieldCondition {
                    key,
                    r#match: Some(Match {
                        match_value: Some(MatchValue::Integer(i)),
                    }),
                    ..Default::default()
                }
            } else if let Some(f) = n.as_f64() {
                // Float equality => use Range.
                FieldCondition {
                    key,
                    range: Some(Range {
                        gte: Some(f),
                        lte: Some(f),
                        ..Default::default()
                    }),
                    ..Default::default()
                }
            } else {
                // Fallback: stringify and match as keyword.
                FieldCondition {
                    key,
                    r#match: Some(Match {
                        match_value: Some(MatchValue::Keyword(n.to_string())),
                    }),
                    ..Default::default()
                }
            }
        }

        J::Bool(b) => FieldCondition {
            key,
            r#match: Some(Match {
                match_value: Some(MatchValue::Boolean(*b)),
            }),
            ..Default::default()
        },

        // Null/Array/Object: fall back to keyword over stringified JSON.
        other => FieldCondition {
            key,
            r#match: Some(Match {
                match_value: Some(MatchValue::Keyword(other.to_string())),
            }),
            ..Default::default()
        },
    };

    // Wrap FieldCondition into ConditionOneOf::Field.
    Condition {
        condition_one_of: Some(condition::ConditionOneOf::Field(field)),
    }
}

/// Small helper for tracing readable filter kind names.
fn kind_of_filter(f: &RagFilter) -> &'static str {
    match f {
        RagFilter::BySource(_) => "BySource",
        RagFilter::ByFieldEq { .. } => "ByFieldEq",
        RagFilter::And(_) => "And",
        RagFilter::Or(_) => "Or",
    }
}
