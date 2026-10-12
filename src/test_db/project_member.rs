use serde::Deserialize;
use worker::{D1Database, Env};

#[derive(Debug, PartialEq, Eq, Deserialize)]
pub struct TestProjectMember {
    pub project_id: u64,
    pub user_id: u64,
    pub username: String,
    pub permission_set: i64,
}

pub async fn insert(db: &D1Database, member: TestProjectMember) {
    worker::query!(
        db,
        "INSERT INTO project_member (project_id, user_id, username, permission_set) VALUES (?, ?, ?, ?)",
        member.project_id,
        member.user_id,
        member.username,
        member.permission_set
    )
    .unwrap()
    .run()
    .await
    .unwrap();
}

pub async fn rows(db: &D1Database) -> Vec<TestProjectMember> {
    db.prepare("SELECT project_id, user_id, username, permission_set FROM project_member ORDER BY project_id, user_id")
        .all()
        .await
        .unwrap()
        .results()
        .unwrap()
}

pub async fn migration_preserves_members(env: Env) {
    let db = env.d1("TEST_DB").unwrap();
    let mut statements = vec![db.prepare("DROP TABLE project_member")];
    statements.extend(
        include_str!("../../migrations/0002_setup.sql")
            .split(';')
            .map(str::trim)
            .filter(|sql| sql.starts_with("create table permission") || sql.starts_with("create index permission_"))
            .map(|sql| db.prepare(sql)),
    );
    statements.extend([
        db.prepare("INSERT INTO project (id, name) VALUES (1, 'alpha'), (2, 'beta')"),
        db.prepare("INSERT INTO user (id) VALUES (1), (2)"),
        db.prepare("INSERT INTO discord_user (id, discord_id, username) VALUES (1, '12345', 'discord.author')"),
        db.prepare("INSERT INTO discord_user (id, username) VALUES (2, 'unlinked.author')"),
        db.prepare("INSERT INTO permission (project_id, user_id, permission_set) VALUES (1, 1, 5), (1, 2, 4), (2, 1, 2)"),
    ]);
    statements.extend(
        include_str!("../../migrations/0004_project_member.sql")
            .split(';')
            .map(str::trim)
            .filter(|sql| !sql.is_empty())
            .map(|sql| db.prepare(sql)),
    );
    db.batch(statements).await.unwrap();
    assert_eq!(
        rows(&db).await,
        vec![
            TestProjectMember {
                project_id: 1,
                user_id: 1,
                username: "discord.author".into(),
                permission_set: 5,
            },
            TestProjectMember {
                project_id: 1,
                user_id: 2,
                username: "unlinked.author".into(),
                permission_set: 4,
            },
            TestProjectMember {
                project_id: 2,
                user_id: 1,
                username: "discord.author".into(),
                permission_set: 2,
            },
        ]
    );
    let old_tables = db
        .prepare("SELECT count(*) AS count FROM sqlite_master WHERE type = 'table' AND name = 'permission'")
        .first::<i64>(Some("count"))
        .await
        .unwrap();
    assert_eq!(old_tables, Some(0));
}
