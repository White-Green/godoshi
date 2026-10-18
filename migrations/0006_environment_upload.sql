create table environment_upload
(
    project_id integer primary key not null,
    upload_key text                not null unique,
    foreign key (project_id) references project (id) on delete cascade
);
