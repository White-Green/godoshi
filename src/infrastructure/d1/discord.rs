use serde::Deserialize;
use smol_str::SmolStr;
use worker::D1Database;
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DiscordUser {
    pub id: UserId,
    pub discord_user_id: SmolStr,
    pub discord_username: SmolStr,
}
use crate::core::user::UserId;

#[cfg(feature = "worker-tests")]
pub mod tests;

pub(super) async fn find(db: &D1Database, discord_user_id: SmolStr, discord_username: SmolStr) -> worker::Result<Option<DiscordUser>> {
    loop {
        let query = worker::query!(
            &db,
            "select id, discord_id, username from discord_user where discord_id = ? or username = ?",
            discord_user_id,
            discord_username
        )
        .expect("Discord user query parameters must be valid D1 values");
        let match_users = query
            .all()
            .await?
            .results::<QueryResult>()
            .expect("Discord user query results must match the schema");
        match match_users.len() {
            1 => {
                let [found_user]: [_; 1] = match_users.try_into().unwrap();
                if found_user.discord_id.as_ref() == Some(&discord_user_id) {
                    if found_user.username != discord_username {
                        worker::query!(&db, "update discord_user set username = ? where id = ?", discord_username, found_user.id,)
                            .expect("Discord username update parameters must be valid D1 values")
                            .await?;
                    }
                    return Ok(Some(DiscordUser {
                        id: found_user.id,
                        discord_user_id,
                        discord_username,
                    }));
                } else if found_user.discord_id.is_none() && found_user.username == discord_username {
                    let result = worker::query!(
                        &db,
                        "update discord_user set discord_id = ? where id = ? and discord_id is null",
                        discord_user_id,
                        found_user.id,
                    )
                    .expect("Discord user link parameters must be valid D1 values")
                    .await?;
                    let rows_written = result
                        .meta()
                        .expect("D1 update metadata must have the expected types")
                        .expect("A successful D1 update must return metadata")
                        .rows_written
                        .expect("D1 update metadata must include rows_written");
                    if rows_written == 1 {
                        return Ok(Some(DiscordUser {
                            id: found_user.id,
                            discord_user_id,
                            discord_username,
                        }));
                    }
                } else {
                    return Ok(None);
                }
            }
            0 | 2.. => return Ok(None), // TODO: 2以上のほうはなんとかしたい...
        }
    }

    #[derive(Debug, Deserialize)]
    struct QueryResult {
        id: UserId,
        discord_id: Option<SmolStr>,
        username: SmolStr,
    }
}
