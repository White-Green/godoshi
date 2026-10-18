use crate::core::user::{ProjectMember, ProjectUserStore, UserPermissionSet, Username};
use smol_str::SmolStr;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DiscordInitMemberError<E> {
    #[error("Duplicated username")]
    DuplicatedUsername,
    #[error("Already a member of this project")]
    AlreadyMember,
    #[error("Error initializing member: {0}")]
    Other(#[source] E),
}

pub trait DiscordUserStore: ProjectUserStore {
    fn sync_discord_user(
        &self,
        discord_user_id: SmolStr,
        discord_username: SmolStr,
    ) -> impl Future<Output = Result<Option<ProjectMember>, <Self as ProjectUserStore>::Error>> + Send;
    fn init_member(
        &self,
        discord_username: Username,
        permissions: UserPermissionSet,
    ) -> impl Future<Output = Result<ProjectMember, DiscordInitMemberError<<Self as ProjectUserStore>::Error>>> + Send;
}
