use crate::user::UserId;
use serde::Deserialize;
use worker::D1Database;

#[derive(Debug, PartialEq, Eq, Deserialize)]
pub struct TestUser {
    pub id: u64,
}

pub async fn insert(db: &D1Database, user: TestUser) -> UserId {
    worker::query!(db, "INSERT INTO user (id) VALUES (?) RETURNING id", user.id)
        .unwrap()
        .first::<UserId>(Some("id"))
        .await
        .unwrap()
        .unwrap()
}
