use crate::project::ProjectId;
use serde::Deserialize;
use smol_str::SmolStr;
use worker::D1Database;

#[derive(Debug, PartialEq, Eq, Deserialize)]
pub struct TestProject {
    pub id: u64,
    pub name: SmolStr,
}

pub async fn insert(db: &D1Database, project: TestProject) -> ProjectId {
    worker::query!(db, "INSERT INTO project (id, name) VALUES (?, ?) RETURNING id", project.id, project.name)
        .unwrap()
        .first::<ProjectId>(Some("id"))
        .await
        .unwrap()
        .unwrap()
}

pub async fn rows(db: &D1Database) -> Vec<TestProject> {
    db.prepare("SELECT id, name FROM project ORDER BY id")
        .all()
        .await
        .unwrap()
        .results()
        .unwrap()
}
