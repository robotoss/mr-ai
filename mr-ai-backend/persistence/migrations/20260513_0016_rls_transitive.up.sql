-- Multi-tenant lift (🅲) sprint C2: transitive RLS policies.
--
-- These tables don't carry `project_id` directly; ownership flows
-- through a parent FK (repo_id → project_repos, review_id →
-- mr_reviews). Each policy uses a subquery against the parent.
--
-- Performance: B-tree on `project_repos(project_id)` exists already;
-- the planner rewrites `IN (SELECT ...)` to a semi-join in modern
-- Postgres so the cost is one index lookup per row. If telemetry
-- shows pressure here, the next step is precomputing the repo set
-- into a session var (e.g. `app.current_tenant_repos`) — out of
-- scope for C2.

-- ============================================================
-- index_state: PK repo_id → project_repos(id)
-- ============================================================
ALTER TABLE index_state ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_iso_index_state ON index_state
    USING (repo_id IN (
        SELECT id FROM project_repos
         WHERE project_id = current_setting('app.current_tenant', true)::uuid
    ));

-- ============================================================
-- graph_nodes: repo_id → project_repos(id)
-- ============================================================
ALTER TABLE graph_nodes ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_iso_graph_nodes ON graph_nodes
    USING (repo_id IN (
        SELECT id FROM project_repos
         WHERE project_id = current_setting('app.current_tenant', true)::uuid
    ));

-- ============================================================
-- graph_edges: from_node + to_node → graph_nodes(id) → repo_id
-- Both endpoints must belong to the same tenant. A cross-tenant
-- edge is invisible to either side; if it ever existed it would be
-- a bug we want to surface, not paper over.
-- ============================================================
ALTER TABLE graph_edges ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_iso_graph_edges ON graph_edges
    USING (
        EXISTS (
            SELECT 1
              FROM graph_nodes gn
              JOIN project_repos pr ON pr.id = gn.repo_id
             WHERE gn.id = graph_edges.from_node
               AND pr.project_id = current_setting('app.current_tenant', true)::uuid
        )
        AND
        EXISTS (
            SELECT 1
              FROM graph_nodes gn
              JOIN project_repos pr ON pr.id = gn.repo_id
             WHERE gn.id = graph_edges.to_node
               AND pr.project_id = current_setting('app.current_tenant', true)::uuid
        )
    );

-- ============================================================
-- mr_review_hypotheses: review_id → mr_reviews(id) → project_id
-- ============================================================
ALTER TABLE mr_review_hypotheses ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_iso_mr_review_hypotheses ON mr_review_hypotheses
    USING (review_id IN (
        SELECT id FROM mr_reviews
         WHERE project_id = current_setting('app.current_tenant', true)::uuid
    ));

-- ============================================================
-- project_dependencies: edges between project_repos(id) endpoints.
-- Tenant must own both endpoints (a dependency that crosses tenants
-- shouldn't exist; if seen, treat as data corruption).
-- ============================================================
ALTER TABLE project_dependencies ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_iso_project_dependencies ON project_dependencies
    USING (
        from_repo_id IN (
            SELECT id FROM project_repos
             WHERE project_id = current_setting('app.current_tenant', true)::uuid
        )
        AND
        to_repo_id IN (
            SELECT id FROM project_repos
             WHERE project_id = current_setting('app.current_tenant', true)::uuid
        )
    );
