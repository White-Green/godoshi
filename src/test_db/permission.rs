use serde::Deserialize;
use worker::D1Database;

#[derive(Debug, PartialEq, Eq, Deserialize)]
pub struct TestPermission {
    pub project_id: u64,
    pub user_id: u64,
    pub permission_set: i64,
}

pub async fn insert(db: &D1Database, permission: TestPermission) {
    worker::query!(
        db,
        "INSERT INTO permission (project_id, user_id, permission_set) VALUES (?, ?, ?)",
        permission.project_id,
        permission.user_id,
        permission.permission_set
    )
    .unwrap()
    .run()
    .await
    .unwrap();
}

pub async fn rows(db: &D1Database) -> Vec<TestPermission> {
    db.prepare("SELECT project_id, user_id, permission_set FROM permission ORDER BY project_id, user_id")
        .all()
        .await
        .unwrap()
        .results()
        .unwrap()
}
