//! Filter conversion to Qdrant `Filter`.
//!
//! Currently supports exact equality on scalar fields (`String`, `Number`, `Bool`).

use crate::record::RagFilter;
use qdrant_client::qdrant::{Condition, FieldCondition, Filter, Match, condition::ConditionOneOf};
use tracing::debug;

/// Converts [`RagFilter`] to Qdrant [`Filter`].
///
/// Currently only supports exact equality for:
/// - `String` → `Keyword`
/// - `Number` → `Integer`
/// - `Bool`   → `Boolean`
pub fn to_qdrant_filter(f: &RagFilter) -> Filter {
    debug!("filters::to_qdrant_filter equals={}", f.equals.len());

    let mut should: Vec<Condition> = Vec::new();

    for (field, val) in &f.equals {
        let m = match val {
            serde_json::Value::String(s) => Match {
                match_value: Some(qdrant_client::qdrant::r#match::MatchValue::Keyword(
                    s.clone(),
                )),
            },
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Match {
                        match_value: Some(qdrant_client::qdrant::r#match::MatchValue::Integer(i)),
                    }
                } else {
                    continue;
                }
            }
            serde_json::Value::Bool(b) => Match {
                match_value: Some(qdrant_client::qdrant::r#match::MatchValue::Boolean(*b)),
            },
            _ => continue, // skip unsupported types
        };

        should.push(Condition {
            condition_one_of: Some(ConditionOneOf::Field(FieldCondition {
                key: field.clone(),
                r#match: Some(m),
                ..Default::default()
            })),
        });
    }

    Filter {
        should,
        ..Default::default()
    }
}
