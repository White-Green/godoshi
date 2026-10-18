use serde::Deserialize;
use worker::D1Database;

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
