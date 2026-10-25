pub mod bucket;
pub mod build;
pub mod environment;
pub mod http;
pub mod metadata;
pub mod project;
pub mod user;

pub trait Config: Send + Sync {
    type Projects: project::ProjectStore;
    type Users: user::UserStore;
    type Bucket: bucket::Bucket;
    type Builds: build::BuildRequests;
    type EnvironmentUploads: environment::EnvironmentUploads;

    fn base_url(&self) -> String;
    fn projects(&self) -> Self::Projects;
    fn users(&self) -> Self::Users;
    fn bucket(&self) -> Self::Bucket;
    fn builds(&self) -> Self::Builds;
    fn environment_uploads(&self) -> Self::EnvironmentUploads;
}
