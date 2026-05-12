-- Per-hypothesis review results. Sprint 4b of 🅰 LLM Quality.
--
-- Today an MR review is one Smart-tier LLM call processing the whole
-- hypothesis list. Review v2 splits that into one call per hypothesis
-- and routes by priority: High/Med → Smart, Low → Fast. The per-call
-- response is validated against a strict JSON schema; the table
-- records every outcome (succeeded / refused / timeout / json_invalid)
-- so we can compute reliability metrics + drive heuristic fallback.
--
-- One row per `(review_id, hypothesis_id)` pair. Reviews can be
-- re-run; the unique constraint protects against double-insert from
-- worker retries.

CREATE TABLE IF NOT EXISTS mr_review_hypotheses (
    id              BIGSERIAL PRIMARY KEY,
    review_id       UUID        NOT NULL REFERENCES mr_reviews(id) ON DELETE CASCADE,
    hypothesis_id   TEXT        NOT NULL,
    priority        SMALLINT    NOT NULL, -- 0=High, 1=Medium, 2=Low
    tier_used       TEXT        NOT NULL, -- "smart" / "fast"
    status          TEXT        NOT NULL, -- "succeeded"/"refused"/"timeout"/"json_invalid"/"heuristic"
    llm_response    JSONB,                -- validated payload or stub
    latency_ms      INTEGER,
    cost_usd        DOUBLE PRECISION,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (review_id, hypothesis_id)
);

CREATE INDEX IF NOT EXISTS mr_review_hypotheses_review_id_idx
    ON mr_review_hypotheses(review_id);
CREATE INDEX IF NOT EXISTS mr_review_hypotheses_status_idx
    ON mr_review_hypotheses(status);
