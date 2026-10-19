pub mod bucket;
pub mod http;
pub mod metadata;
pub mod project;
pub mod user;

pub trait Config: Send + Sync {
    type Projects: project::ProjectStore;
    type Users: user::UserStore;
    type Bucket: bucket::Bucket;

    fn base_url(&self) -> String;
    fn projects(&self) -> Self::Projects;
    fn users(&self) -> Self::Users;
    fn bucket(&self) -> Self::Bucket;
}
