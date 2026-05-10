-- Code graph: directed edges between graph_nodes.

CREATE TABLE IF NOT EXISTS graph_edges (
    id          BIGSERIAL PRIMARY KEY,
    from_node   UUID NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
    to_node     UUID NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
    edge_type   TEXT NOT NULL,
    weight      REAL NOT NULL DEFAULT 1.0,
    -- Optional metadata payload (call-site row for Calls, import alias for
    -- Imports, etc.). Bounded — store nothing larger than a few hundred bytes.
    meta        JSONB,
    UNIQUE (from_node, to_node, edge_type)
);

CREATE INDEX IF NOT EXISTS graph_edges_from_idx ON graph_edges(from_node);
CREATE INDEX IF NOT EXISTS graph_edges_to_idx   ON graph_edges(to_node);
CREATE INDEX IF NOT EXISTS graph_edges_type_idx ON graph_edges(edge_type);
