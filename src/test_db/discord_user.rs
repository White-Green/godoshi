use serde::Deserialize;
use smol_str::SmolStr;
use worker::D1Database;

#[derive(Debug, PartialEq, Eq, Deserialize)]
pub struct TestDiscordUser {
    pub id: u64,
    pub discord_id: Option<SmolStr>,
    pub username: String,
}

pub async fn insert(db: &D1Database, user: TestDiscordUser) {
    db.batch(vec![
        worker::query!(db, "INSERT INTO user (id) VALUES (?)", user.id).unwrap(),
        worker::query!(
            db,
            "INSERT INTO discord_user (id, discord_id, username) VALUES (?, ?, ?)",
            user.id,
            user.discord_id,
            user.username
        )
        .unwrap(),
    ])
    .await
    .unwrap();
}

pub async fn rows(db: &D1Database) -> Vec<TestDiscordUser> {
    db.prepare("SELECT id, discord_id, username FROM discord_user ORDER BY id")
        .all()
        .await
        .unwrap()
        .results()
        .unwrap()
}
