use crate::user::UserId;
use serde::Deserialize;
use smol_str::SmolStr;
use worker::D1Database;

#[cfg(feature = "worker-tests")]
pub mod tests;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordUser {
    pub id: UserId,
    pub discord_user_id: SmolStr,
    pub discord_username: SmolStr,
}

pub async fn find(db: &D1Database, discord_user_id: SmolStr, discord_username: SmolStr) -> Option<DiscordUser> {
    loop {
        let query = worker::query!(
            &db,
            "select id, discord_id, username from discord_user where discord_id = ? or username = ?",
            discord_user_id,
            discord_username
        )
        .unwrap();
        let match_users = query.all().await.unwrap().results::<QueryResult>().unwrap();
        match match_users.len() {
            1 => {
                let [found_user]: [_; 1] = match_users.try_into().unwrap();
                if found_user.discord_id.as_ref() == Some(&discord_user_id) {
                    if found_user.username != discord_username {
                        worker::query!(&db, "update discord_user set username = ? where id = ?", discord_username, found_user.id,)
                            .unwrap()
                            .await
                            .unwrap();
                    }
                    return Some(DiscordUser {
                        id: found_user.id,
                        discord_user_id,
                        discord_username,
                    });
                } else if found_user.discord_id.is_none() && found_user.username == discord_username {
                    let result = worker::query!(
                        &db,
                        "update discord_user set discord_id = ? where id = ? and discord_id is null",
                        discord_user_id,
                        found_user.id,
                    )
                    .unwrap()
                    .await
                    .unwrap();
                    if result.meta().unwrap().unwrap().rows_written == Some(1) {
                        return Some(DiscordUser {
                            id: found_user.id,
                            discord_user_id,
                            discord_username,
                        });
                    }
                } else {
                    return None;
                }
            }
            0 | 2.. => return None, // TODO: 2以上のほうはなんとかしたい...
        }
    }

    #[derive(Debug, Deserialize)]
    struct QueryResult {
        id: UserId,
        discord_id: Option<SmolStr>,
        username: SmolStr,
    }
}
