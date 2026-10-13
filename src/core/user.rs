use crate::core::project::ProjectId;
use crate::core::user::discord::DiscordUserStore;
use bitflags::bitflags;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use smol_str::SmolStr;
use std::error::Error;
use std::fmt;
use std::fmt::Display;

pub mod discord;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UserId(u64);

impl UserId {
    pub const fn new(id: u64) -> Self {
        Self(id)
    }
}

impl Display for UserId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Username(SmolStr);

impl Username {
    pub fn new(username: SmolStr) -> Option<Username> {
        if username.is_empty() || username.len() > 32 || matches!(username.as_str(), "." | "..") {
            return None;
        }
        if !username.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
            return None;
        }
        Some(Username(username))
    }
}

impl<'de> Deserialize<'de> for Username {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(SmolStr::deserialize(deserializer)?).ok_or_else(|| serde::de::Error::custom("invalid username"))
    }
}

impl Display for Username {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

impl AsRef<str> for Username {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct UserPermissionSet: u8 {
        const ADMIN       = 0b001;
        const REVIEWER    = 0b010;
        const PARTICIPANT = 0b100;
    }
}

impl Serialize for UserPermissionSet {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_i64(self.bits() as i64)
    }
}

impl<'de> Deserialize<'de> for UserPermissionSet {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bits = i64::deserialize(deserializer)?;
        let Ok(bits) = u8::try_from(bits) else {
            return Err(serde::de::Error::custom("invalid permission bits"));
        };
        UserPermissionSet::from_bits(bits).ok_or_else(|| serde::de::Error::custom("invalid permission bits"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectMember {
    user_id: UserId,
    username: Username,
    permissions: UserPermissionSet,
}

impl ProjectMember {
    pub fn new(user_id: UserId, username: Username, permissions: UserPermissionSet) -> Self {
        Self {
            user_id,
            username,
            permissions,
        }
    }
    pub fn user_id(&self) -> UserId {
        self.user_id
    }

    pub fn username(&self) -> &Username {
        &self.username
    }

    pub fn permissions(&self) -> UserPermissionSet {
        self.permissions
    }

    pub fn is_admin(&self) -> bool {
        self.permissions.contains(UserPermissionSet::ADMIN)
    }

    pub fn is_participant(&self) -> bool {
        self.permissions.contains(UserPermissionSet::PARTICIPANT)
    }

    pub fn is_reviewer(&self) -> bool {
        self.permissions.contains(UserPermissionSet::REVIEWER)
    }
}

pub trait UserStore: Send + Sync {
    type DiscordUserStore<'a>: DiscordUserStore + 'a
    where
        Self: 'a;
    fn project(&self, project: ProjectId) -> Self::DiscordUserStore<'_>;
}

/// 特定のプロジェクトに所属しているユーザ群
pub trait ProjectUserStore: Send + Sync {
    type Error: Error + Send + Sync + 'static;
    fn find_member(&self, user_id: UserId) -> impl Future<Output = Result<Option<ProjectMember>, Self::Error>> + Send;
    fn all_member(&self) -> impl Future<Output = Result<Vec<ProjectMember>, Self::Error>> + Send;
    fn update_permission(&self, user_id: UserId, permissions: UserPermissionSet) -> impl Future<Output = Result<bool, Self::Error>> + Send;
}
