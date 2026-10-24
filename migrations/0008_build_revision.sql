ALTER TABLE project ADD COLUMN build_revision INTEGER NOT NULL DEFAULT 0 CHECK (build_revision >= 0);
ALTER TABLE project ADD COLUMN started_build_revision INTEGER NOT NULL DEFAULT 0
    CHECK (started_build_revision >= 0 AND started_build_revision <= build_revision);
