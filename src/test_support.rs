#![allow(unused)]
use crate::core::Config;
use crate::core::project::{Project, ProjectId, ProjectName, ProjectStore};
use crate::core::user::discord::{DiscordInitMemberError, DiscordUserStore};
use crate::core::user::{ProjectMember, ProjectUserStore, UserId, UserPermissionSet, UserStore, Username};
use smol_str::SmolStr;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

mod bucket;
pub use bucket::MemoryBucket;

pub fn username(name: &str) -> Username {
    Username::new(name.into()).unwrap()
}

#[derive(Clone, Default)]
pub struct TestConfig {
    db: MemoryDb,
    bucket: MemoryBucket,
    base_url: Option<String>,
}
impl TestConfig {
    pub fn db(&self) -> MemoryDb {
        self.db.clone()
    }
    pub fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = Some(base_url);
        self
    }
}
impl Config for TestConfig {
    type Projects = MemoryDb;
    type Users = MemoryDb;
    type Bucket = MemoryBucket;
    type Builds = MemoryDb;
    type EnvironmentUploads = MemoryDb;
    fn environment_uploads(&self) -> MemoryDb {
        self.db.clone()
    }
    fn builds(&self) -> MemoryDb {
        self.db.clone()
    }
    fn base_url(&self) -> String {
        self.base_url.clone().unwrap_or_else(|| "https://godoshi.test".into())
    }
    fn projects(&self) -> MemoryDb {
        self.db.clone()
    }
    fn users(&self) -> MemoryDb {
        self.db.clone()
    }
    fn bucket(&self) -> MemoryBucket {
        self.bucket.with_db(self.db.clone())
    }
}

#[derive(Default)]
struct Records {
    build_requests: Vec<ProjectId>,
    metadata_schemas: BTreeMap<ProjectId, crate::core::metadata::MetadataSchema>,
    projects: BTreeMap<u64, project::TestProject>,
    environment_uploads: BTreeMap<ProjectId, String>,
    users: BTreeMap<u64, discord_user::TestDiscordUser>,
    members: BTreeMap<(u64, u64), project_member::TestProjectMember>,
    next_id: u64,
}
#[derive(Clone, Default)]
pub struct MemoryDb(Arc<Mutex<Records>>);
impl MemoryDb {
    pub fn build_requests(&self) -> Vec<ProjectId> {
        self.0.lock().unwrap().build_requests.clone()
    }
    pub fn rename_member(&self, project: u64, user: u64, name: &str) {
        self.0.lock().unwrap().members.get_mut(&(project, user)).unwrap().username = name.into();
    }
    pub fn rename_discord(&self, user: u64, name: &str) {
        self.0.lock().unwrap().users.get_mut(&user).unwrap().username = name.into();
    }
}

impl crate::core::environment::EnvironmentUploads for MemoryDb {
    type Error = std::io::Error;
    fn presign(&self, project: ProjectId) -> Result<crate::core::environment::EnvironmentUpload, Self::Error> {
        Ok(crate::core::environment::EnvironmentUpload {
            url: format!("https://r2.test/bucket/projects/{project}/resources/environment.closure_zstd?signature=test"),
            expires_in: 900,
        })
    }
}

impl crate::core::build::BuildRequests for MemoryDb {
    type Error = std::io::Error;
    async fn request(&self, project: ProjectId) -> Result<(), Self::Error> {
        self.0.lock().unwrap().build_requests.push(project);
        Ok(())
    }
}

pub struct MemoryUsers {
    db: MemoryDb,
    project: ProjectId,
}
impl UserStore for MemoryDb {
    type DiscordUserStore<'a> = MemoryUsers;
    fn project(&self, project: ProjectId) -> MemoryUsers {
        MemoryUsers { db: self.clone(), project }
    }
}
impl ProjectStore for MemoryDb {
    type Error = std::io::Error;
    async fn metadata_schema(&self, project: ProjectId) -> Result<crate::core::metadata::MetadataSchema, Self::Error> {
        Ok(self.0.lock().unwrap().metadata_schemas.get(&project).cloned().unwrap_or_default())
    }
    async fn save_metadata_schema(&self, project: ProjectId, schema: crate::core::metadata::MetadataSchema) -> Result<(), Self::Error> {
        schema.validate().map_err(std::io::Error::other)?;
        self.0.lock().unwrap().metadata_schemas.insert(project, schema);
        Ok(())
    }
    async fn find_project(&self, name: &str) -> Result<Option<Project>, Self::Error> {
        Ok(self.0.lock().unwrap().projects.values().find(|p| p.name == name).map(|p| Project {
            id: ProjectId::new(p.id),
            name: ProjectName::new(p.name.clone()).unwrap(),
        }))
    }
    async fn environment_upload_key(&self, project: ProjectId) -> Result<Option<String>, Self::Error> {
        Ok(self.0.lock().unwrap().environment_uploads.get(&project).cloned())
    }
    async fn create_environment_upload_key(&self, project: ProjectId, key: &str) -> Result<(), Self::Error> {
        self.0
            .lock()
            .unwrap()
            .environment_uploads
            .entry(project)
            .or_insert_with(|| key.to_owned());
        Ok(())
    }
    async fn delete_environment_upload_key(&self, project: ProjectId, key: &str) -> Result<(), Self::Error> {
        let mut records = self.0.lock().unwrap();
        if records.environment_uploads.get(&project).is_some_and(|stored| stored == key) {
            records.environment_uploads.remove(&project);
        }
        Ok(())
    }
    async fn find_environment_upload_project(&self, key: &str) -> Result<Option<ProjectId>, Self::Error> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .environment_uploads
            .iter()
            .find_map(|(project, stored)| (stored == key).then_some(*project)))
    }
}
fn member(row: &project_member::TestProjectMember) -> ProjectMember {
    ProjectMember::new(
        UserId::new(row.user_id),
        username(&row.username),
        UserPermissionSet::from_bits(row.permission_set as u8).unwrap(),
    )
}
impl ProjectUserStore for MemoryUsers {
    type Error = std::io::Error;
    async fn find_member(&self, user: UserId) -> Result<Option<ProjectMember>, Self::Error> {
        Ok(self
            .db
            .0
            .lock()
            .unwrap()
            .members
            .values()
            .find(|m| ProjectId::new(m.project_id) == self.project && UserId::new(m.user_id) == user)
            .map(member))
    }
    async fn all_member(&self) -> Result<Vec<ProjectMember>, Self::Error> {
        let mut members: Vec<_> = self
            .db
            .0
            .lock()
            .unwrap()
            .members
            .values()
            .filter(|m| ProjectId::new(m.project_id) == self.project)
            .map(member)
            .collect();
        members.sort_by(|a, b| (a.username(), a.user_id()).cmp(&(b.username(), b.user_id())));
        Ok(members)
    }
    async fn update_permission(&self, user: UserId, permissions: UserPermissionSet) -> Result<bool, Self::Error> {
        let mut records = self.db.0.lock().unwrap();
        let row = records
            .members
            .values_mut()
            .find(|m| ProjectId::new(m.project_id) == self.project && UserId::new(m.user_id) == user);
        if let Some(row) = row {
            row.permission_set = permissions.bits().into();
            Ok(true)
        } else {
            Ok(false)
        }
    }
}
impl DiscordUserStore for MemoryUsers {
    async fn sync_discord_user(&self, id: SmolStr, name: SmolStr) -> Result<Option<ProjectMember>, Self::Error> {
        let user = {
            let mut records = self.db.0.lock().unwrap();
            let matches: Vec<_> = records
                .users
                .values()
                .filter(|u| u.discord_id.as_ref() == Some(&id) || u.username == name)
                .map(|u| u.id)
                .collect();
            let [user_id] = matches.as_slice() else {
                return Ok(None);
            };
            let user = records.users.get_mut(user_id).unwrap();
            if user.discord_id.is_some() && user.discord_id.as_ref() != Some(&id) {
                return Ok(None);
            }
            user.discord_id = Some(id);
            user.username = name.to_string();
            UserId::new(*user_id)
        };
        self.find_member(user).await
    }
    async fn init_member(&self, name: Username, permissions: UserPermissionSet) -> Result<ProjectMember, DiscordInitMemberError<Self::Error>> {
        use DiscordInitMemberError::*;
        let mut records = self.db.0.lock().unwrap();
        let ids: Vec<_> = records.users.values().filter(|u| u.username == name.as_ref()).map(|u| u.id).collect();
        let id = match ids.as_slice() {
            [] => {
                records.next_id += 1;
                let id = records.next_id;
                records.users.insert(
                    id,
                    discord_user::TestDiscordUser {
                        id,
                        discord_id: None,
                        username: name.to_string(),
                    },
                );
                id
            }
            [id] => *id,
            _ => return Err(DuplicatedUsername),
        };
        let project = records.projects.keys().find(|id| ProjectId::new(**id) == self.project).copied().unwrap();
        if records.members.contains_key(&(project, id)) {
            return Err(AlreadyMember);
        }
        let row = project_member::TestProjectMember {
            project_id: project,
            user_id: id,
            username: name.to_string(),
            permission_set: permissions.bits().into(),
        };
        let result = member(&row);
        records.members.insert((project, id), row);
        Ok(result)
    }
}

pub mod project {
    use super::*;
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct TestProject {
        pub id: u64,
        pub name: SmolStr,
    }
    pub async fn insert(db: &MemoryDb, project: TestProject) -> ProjectId {
        let id = ProjectId::new(project.id);
        db.0.lock().unwrap().projects.insert(project.id, project);
        id
    }
}
pub mod user {
    use super::*;
    pub struct TestUser {
        pub id: u64,
    }
    pub async fn insert(db: &MemoryDb, user: TestUser) -> UserId {
        let mut records = db.0.lock().unwrap();
        records.next_id = records.next_id.max(user.id);
        UserId::new(user.id)
    }
}
pub mod discord_user {
    use super::*;
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct TestDiscordUser {
        pub id: u64,
        pub discord_id: Option<SmolStr>,
        pub username: String,
    }
    pub async fn insert(db: &MemoryDb, user: TestDiscordUser) {
        let mut records = db.0.lock().unwrap();
        records.next_id = records.next_id.max(user.id);
        records.users.insert(user.id, user);
    }
    pub async fn rows(db: &MemoryDb) -> Vec<TestDiscordUser> {
        db.0.lock().unwrap().users.values().cloned().collect()
    }
}
pub mod project_member {
    use super::*;
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct TestProjectMember {
        pub project_id: u64,
        pub user_id: u64,
        pub username: String,
        pub permission_set: i64,
    }
    pub async fn insert(db: &MemoryDb, member: TestProjectMember) {
        db.0.lock().unwrap().members.insert((member.project_id, member.user_id), member);
    }
    pub async fn rows(db: &MemoryDb) -> Vec<TestProjectMember> {
        db.0.lock().unwrap().members.values().cloned().collect()
    }
}
