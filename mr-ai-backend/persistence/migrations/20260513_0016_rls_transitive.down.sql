DROP POLICY IF EXISTS tenant_iso_index_state          ON index_state;
DROP POLICY IF EXISTS tenant_iso_graph_nodes          ON graph_nodes;
DROP POLICY IF EXISTS tenant_iso_graph_edges          ON graph_edges;
DROP POLICY IF EXISTS tenant_iso_mr_review_hypotheses ON mr_review_hypotheses;
DROP POLICY IF EXISTS tenant_iso_project_dependencies ON project_dependencies;

ALTER TABLE index_state          DISABLE ROW LEVEL SECURITY;
ALTER TABLE graph_nodes          DISABLE ROW LEVEL SECURITY;
ALTER TABLE graph_edges          DISABLE ROW LEVEL SECURITY;
ALTER TABLE mr_review_hypotheses DISABLE ROW LEVEL SECURITY;
ALTER TABLE project_dependencies DISABLE ROW LEVEL SECURITY;
