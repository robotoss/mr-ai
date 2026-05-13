-- Idempotency log for inbound webhooks.

CREATE TABLE IF NOT EXISTS webhook_events (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    provider      TEXT NOT NULL CHECK (provider IN ('gitlab','github','bitbucket')),
    event_id      TEXT NOT NULL,
    event_kind    TEXT NOT NULL,
    payload_hash  BYTEA NOT NULL,
    payload       JSONB NOT NULL,
    received_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    status        TEXT NOT NULL DEFAULT 'received'
        CHECK (status IN ('received','enqueued','rejected','failed')),
    UNIQUE (provider, event_id)
);

CREATE INDEX IF NOT EXISTS webhook_events_received_at_idx
    ON webhook_events(received_at DESC);
