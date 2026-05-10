-- Extend index_state with a directory checkpoint so a Reindex job can
-- resume after timeout without re-walking the full worktree (S2 emits
-- it as NULL; S9 wires actual auto-split and resume).

ALTER TABLE index_state
    ADD COLUMN IF NOT EXISTS last_indexed_path_prefix TEXT;
