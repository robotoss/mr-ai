-- Multi-tenant lift (🅲) sprint C4: tighten `audit_log.project_id`
-- to NOT NULL. The middleware in C3 always knows the tenant scope
-- before letting the handler emit; webhooks resolve project_id from
-- payload via the HMAC-verified `find_repo_by_remote_url` path.
-- Both paths now have a project_id by the time an audit row lands.
--
-- Any pre-existing NULL rows are pre-multi-tenant data; deleting
-- them removes the only legal NULL → NOT NULL transition. Audit log
-- is best-effort historical data — losing pre-tenant rows is
-- acceptable and clears the way for the constraint.

DELETE FROM audit_log WHERE project_id IS NULL;

ALTER TABLE audit_log ALTER COLUMN project_id SET NOT NULL;
