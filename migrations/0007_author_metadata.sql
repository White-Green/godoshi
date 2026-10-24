CREATE TABLE project_metadata_schema (
    project_id INTEGER PRIMARY KEY REFERENCES project (id) ON DELETE RESTRICT,
    definition TEXT NOT NULL CHECK (json_valid(definition) AND json_type(definition) = 'object')
);

CREATE TABLE author_metadata (
    project_id INTEGER NOT NULL,
    user_id INTEGER NOT NULL,
    values_json TEXT NOT NULL CHECK (json_valid(values_json) AND json_type(values_json) = 'object'),
    assets TEXT NOT NULL CHECK (json_valid(assets) AND json_type(assets) = 'object'),
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, user_id),
    FOREIGN KEY (project_id, user_id) REFERENCES project_member (project_id, user_id) ON DELETE RESTRICT
);
