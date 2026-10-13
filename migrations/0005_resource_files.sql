CREATE UNIQUE INDEX project_member_project_username ON project_member (project_id, username);

CREATE TABLE resource_file_list (
    project_id INTEGER NOT NULL,
    user_id INTEGER NOT NULL,
    files TEXT NOT NULL CHECK (json_valid(files) AND json_type(files) = 'array' AND json_array_length(files) <= 100),
    revision INTEGER NOT NULL,
    PRIMARY KEY (project_id, user_id),
    FOREIGN KEY (project_id, user_id) REFERENCES project_member (project_id, user_id) ON DELETE RESTRICT
);
