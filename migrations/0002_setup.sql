create table project
(
    id   integer primary key autoincrement not null,
    name text                              not null
);
create unique index project_id on project (id);
create unique index project_name on project (name);

create table user
(
    id integer primary key autoincrement not null
);

create table discord_user
(
    id         integer primary key not null,
    discord_id text,
    username   text                not null,
    foreign key (id) references user (id) on delete restrict
);
create unique index discord_user_id on discord_user (id);
create unique index discord_user_discord_id on discord_user (discord_id) where discord_id is not null;
create index discord_user_username on discord_user (username);

create table permission
(
    project_id     integer not null,
    user_id        integer not null,
    permission_set integer not null,
    foreign key (project_id) references project (id) on delete restrict,
    foreign key (user_id) references user (id) on delete restrict,
    primary key (project_id, user_id)
);
create index permission_project_id on permission (project_id);
