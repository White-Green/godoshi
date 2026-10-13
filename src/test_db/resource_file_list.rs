use serde::Deserialize;
use worker::D1Database;

#[derive(Debug, PartialEq, Eq, Deserialize)]
pub struct ResourceFileList {
    pub project_id: u64,
    pub user_id: u64,
    pub files: String,
    pub revision: u64,
}

pub async fn insert(db: &D1Database, member: ResourceFileList) {
    worker::query!(
        db,
        "INSERT INTO resource_file_list (project_id, user_id, files, revision) VALUES (?, ?, ?, ?)",
        member.project_id,
        member.user_id,
        member.files,
        member.revision
    )
    .unwrap()
    .run()
    .await
    .unwrap();
}

pub async fn rows(db: &D1Database) -> Vec<ResourceFileList> {
    db.prepare("SELECT project_id, user_id, files, revision FROM resource_file_list ORDER BY project_id, user_id")
        .all()
        .await
        .unwrap()
        .results()
        .unwrap()
}
