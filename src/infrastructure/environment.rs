use crate::core::build::BuildRequests;
use crate::core::environment::{ENVIRONMENT_FILE, EnvironmentUpload, EnvironmentUploads};
use crate::core::project::ProjectId;
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use serde::Deserialize;
use std::time::Duration;
use worker::Env;

const EXPIRES_IN: u32 = 180;

pub struct R2EnvironmentUploads(pub Env);

impl EnvironmentUploads for R2EnvironmentUploads {
    type Error = std::io::Error;

    fn presign(&self, project: ProjectId) -> Result<EnvironmentUpload, Self::Error> {
        let var = |name| {
            self.0
                .var(name)
                .map(|value| value.to_string())
                .map_err(|_| std::io::Error::other(format!("Missing R2 configuration: {name}")))
        };
        let secret = |name| {
            self.0
                .secret(name)
                .map(|value| value.to_string())
                .map_err(|_| std::io::Error::other(format!("Missing R2 secret: {name}")))
        };
        presign(
            &var("R2_ACCOUNT_ID")?,
            &var("R2_BUCKET_NAME")?,
            &secret("R2_ACCESS_KEY_ID")?,
            &secret("R2_SECRET_ACCESS_KEY")?,
            project,
        )
    }
}

fn presign(account: &str, bucket: &str, access_key: &str, secret_key: &str, project: ProjectId) -> Result<EnvironmentUpload, std::io::Error> {
    let endpoint = format!("https://{account}.r2.cloudflarestorage.com").parse().unwrap();
    let bucket = Bucket::new(endpoint, UrlStyle::Path, bucket.to_owned(), "auto").unwrap();
    let credentials = Credentials::new(access_key, secret_key);
    let key = format!("projects/{project}/resources/{ENVIRONMENT_FILE}");
    let url = bucket
        .put_object(Some(&credentials), &key)
        .sign(Duration::from_secs(u64::from(EXPIRES_IN)));
    Ok(EnvironmentUpload {
        url: url.into(),
        expires_in: EXPIRES_IN,
    })
}

#[derive(Deserialize)]
pub struct EnvironmentUploaded {
    account: String,
    bucket: String,
    action: String,
    object: UploadedObject,
}

#[derive(Deserialize)]
struct UploadedObject {
    key: String,
}

impl EnvironmentUploaded {
    fn project(&self, account: &str, bucket: &str) -> Option<ProjectId> {
        if self.account != account || self.bucket != bucket || self.action != "PutObject" {
            return None;
        }
        let project = self
            .object
            .key
            .strip_prefix("projects/")?
            .strip_suffix(&format!("/resources/{ENVIRONMENT_FILE}"))?;
        let id = project.parse::<u64>().ok()?;
        (project == id.to_string()).then_some(ProjectId::new(id))
    }

    pub async fn request_build<B: BuildRequests>(&self, account: &str, bucket: &str, builds: &B) -> Result<(), B::Error> {
        if let Some(project) = self.project(account, bucket) {
            builds.request(project).await?;
        }
        Ok(())
    }
}
