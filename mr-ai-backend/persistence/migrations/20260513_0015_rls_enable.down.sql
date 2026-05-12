DROP POLICY IF EXISTS tenant_iso_projects        ON projects;
DROP POLICY IF EXISTS tenant_iso_project_repos   ON project_repos;
DROP POLICY IF EXISTS tenant_iso_jobs            ON jobs;
DROP POLICY IF EXISTS tenant_iso_mr_reviews      ON mr_reviews;
DROP POLICY IF EXISTS tenant_iso_audit_log       ON audit_log;
DROP POLICY IF EXISTS tenant_iso_secrets_metadata ON secrets_metadata;

ALTER TABLE projects         DISABLE ROW LEVEL SECURITY;
ALTER TABLE project_repos    DISABLE ROW LEVEL SECURITY;
ALTER TABLE jobs             DISABLE ROW LEVEL SECURITY;
ALTER TABLE mr_reviews       DISABLE ROW LEVEL SECURITY;
ALTER TABLE audit_log        DISABLE ROW LEVEL SECURITY;
ALTER TABLE secrets_metadata DISABLE ROW LEVEL SECURITY;
