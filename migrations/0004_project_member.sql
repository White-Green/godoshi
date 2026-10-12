create table project_member
(
    project_id     integer not null,
    user_id        integer not null,
    username       text    not null,
    permission_set integer not null,
    foreign key (project_id) references project (id) on delete restrict,
    foreign key (user_id) references user (id) on delete restrict,
    primary key (project_id, user_id)
);
create index project_member_project_id on project_member (project_id);

insert into project_member (project_id, user_id, username, permission_set)
select p.project_id, p.user_id, d.username, p.permission_set
from permission p
left join discord_user d on d.id = p.user_id;

drop table permission;
