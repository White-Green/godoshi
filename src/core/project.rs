use serde::{Deserialize, Deserializer, Serialize};
use smol_str::SmolStr;
use std::error::Error;
use std::fmt;
use std::fmt::{Debug, Display};

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ProjectName(SmolStr);

impl ProjectName {
    pub fn new(name: SmolStr) -> Option<ProjectName> {
        (!name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')).then_some(ProjectName(name))
    }
}

impl<'de> Deserialize<'de> for ProjectName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let name = SmolStr::deserialize(deserializer)?;
        ProjectName::new(name).ok_or_else(|| serde::de::Error::custom("invalid project name"))
    }
}

impl Debug for ProjectName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Debug::fmt(&self.0, f)
    }
}

impl Display for ProjectName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

impl AsRef<str> for ProjectName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl PartialEq<str> for ProjectName {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProjectId(u64);

impl ProjectId {
    pub const fn new(id: u64) -> Self {
        Self(id)
    }
}

impl Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    pub name: ProjectName,
}

impl Project {
    pub fn id(&self) -> ProjectId {
        self.id
    }

    pub fn name(&self) -> &ProjectName {
        &self.name
    }
}

pub trait ProjectStore: Send + Sync {
    type Error: Error + Send + Sync + 'static;
    fn find_project(&self, name: &str) -> impl Future<Output = Result<Option<Project>, Self::Error>> + Send;
}
