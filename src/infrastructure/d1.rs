use crate::core::project::{Project, ProjectId, ProjectStore};
use crate::core::user::discord::{DiscordInitMemberError, DiscordUserStore};
use crate::core::user::{ProjectMember, ProjectUserStore, UserId, UserPermissionSet, UserStore, Username};
use smol_str::SmolStr;
use worker::D1Database;

pub mod discord;
#[cfg(feature = "worker-tests")]
pub mod tests;

pub struct D1Store(pub D1Database);

pub struct D1ProjectUsers<'a> {
    db: &'a D1Database,
    project: ProjectId,
}

fn error(error: worker::Error) -> std::io::Error {
    std::io::Error::other(error.to_string())
}

impl ProjectStore for D1Store {
    type Error = std::io::Error;
    #[worker::send]
    async fn find_project(&self, name: &str) -> Result<Option<Project>, Self::Error> {
        worker::query!(&self.0, "select id, name from project where name = ?", name)
            .expect("Project query parameters must be valid D1 values")
            .first(None)
            .await
            .map_err(error)
    }
}

impl UserStore for D1Store {
    type DiscordUserStore<'a> = D1ProjectUsers<'a>;
    fn project(&self, project: ProjectId) -> Self::DiscordUserStore<'_> {
        D1ProjectUsers { db: &self.0, project }
    }
}

impl ProjectUserStore for D1ProjectUsers<'_> {
    type Error = std::io::Error;
    #[worker::send]
    async fn find_member(&self, user_id: UserId) -> Result<Option<ProjectMember>, Self::Error> {
        worker::query!(
            self.db,
            "select user_id, username, permission_set as permissions from project_member where project_id = ? and user_id = ?",
            self.project,
            user_id
        )
        .expect("Member query parameters must be valid D1 values")
        .first(None)
        .await
        .map_err(error)
    }
    #[worker::send]
    async fn all_member(&self) -> Result<Vec<ProjectMember>, Self::Error> {
        worker::query!(
            self.db,
            "select user_id, username, permission_set as permissions from project_member where project_id = ? order by username, user_id",
            self.project
        )
        .expect("Member query parameters must be valid D1 values")
        .all()
        .await
        .map_err(error)?
        .results()
        .map_err(error)
    }
    #[worker::send]
    async fn update_permission(&self, user_id: UserId, permissions: UserPermissionSet) -> Result<bool, Self::Error> {
        worker::query!(
            self.db,
            "update project_member set permission_set = ? where project_id = ? and user_id = ? returning user_id",
            permissions,
            self.project,
            user_id
        )
        .expect("Permission update parameters must be valid D1 values")
        .first::<UserId>(Some("user_id"))
        .await
        .map(|id| id.is_some())
        .map_err(error)
    }
}

impl DiscordUserStore for D1ProjectUsers<'_> {
    #[worker::send]
    async fn sync_discord_user(&self, discord_user_id: SmolStr, discord_username: SmolStr) -> Result<Option<ProjectMember>, Self::Error> {
        let Some(user) = discord::find(self.db, discord_user_id, discord_username).await.map_err(error)? else {
            return Ok(None);
        };
        self.find_member(user.id).await
    }
    #[worker::send]
    async fn init_member(&self, username: Username, permissions: UserPermissionSet) -> Result<ProjectMember, DiscordInitMemberError<Self::Error>> {
        use DiscordInitMemberError::*;
        let result = worker::query!(self.db, "select id from discord_user where username = ?", username)
            .expect("Username query parameters must be valid D1 values")
            .all()
            .await
            .map_err(|e| Other(error(e)))?;
        if result.results::<serde_json::Value>().map_err(|e| Other(error(e)))?.len() > 1 {
            return Err(DuplicatedUsername);
        }
        // D1 batches are atomic. Reuse identities and preserve permissions on repeated submissions.
        let results = self
            .db
            .batch(vec![
                worker::query!(
                    self.db,
                    "insert into user (id) select null where not exists (select 1 from discord_user where username = ?)",
                    username
                )
                .unwrap(),
                worker::query!(
                    self.db,
                    "insert into discord_user (id, username) select last_insert_rowid(), ? where changes() = 1",
                    username
                )
                .unwrap(),
                worker::query!(
                    self.db,
                    "insert into project_member (project_id, user_id, username, permission_set)
                 select ?, id, username, ? from discord_user where username = ?
                 on conflict (project_id, user_id) do nothing returning user_id, username, permission_set as permissions",
                    self.project,
                    permissions,
                    username
                )
                .unwrap(),
            ])
            .await
            .map_err(|e| Other(error(e)))?;
        results[2]
            .results::<ProjectMember>()
            .map_err(|e| Other(error(e)))?
            .into_iter()
            .next()
            .ok_or(AlreadyMember)
    }
}
