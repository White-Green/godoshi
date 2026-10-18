use crate::core::bucket::{Bucket, ProjectBucket, ResourceBucket, ResultBucket, ResultKey, ResultListBucket, S3_RESOURCES_BUCKET, S3_RESULTS_BUCKET};
use crate::core::project::ProjectId;
use s3s::dto::*;
use s3s::service::S3ServiceBuilder;
use s3s::{S3, S3Error, S3ErrorCode, S3Request, S3Response, S3Result};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct Params {
    pub project_id: u64,
    pub run_id: String,
}

pub async fn handle(request: s3s::HttpRequest, bucket: &impl Bucket, params: Params) -> S3Result<s3s::HttpResponse> {
    let key = ResultKey::new(params.run_id).ok_or_else(|| S3Error::new(S3ErrorCode::InvalidArgument))?;
    let project = bucket.project(ProjectId::new(params.project_id));
    let resources = project.resource().as_s3();
    let results = project.result().result(&key).as_s3();
    S3ServiceBuilder::new(ProjectStorage { resources, results })
        .build()
        .call(request)
        .await
        .map_err(|error| {
            tracing::error!(?error, "S3 protocol handler failed");
            S3Error::with_source(S3ErrorCode::InternalError, error.into_std_error())
        })
}

struct ProjectStorage<Resources, Results> {
    resources: Resources,
    results: Results,
}

impl<Resources, Results> ProjectStorage<Resources, Results>
where
    Resources: S3,
    Results: S3,
{
    fn read(&self, bucket: &str) -> S3Result<&impl S3> {
        if bucket != S3_RESOURCES_BUCKET {
            return Err(S3Error::new(S3ErrorCode::AccessDenied));
        }
        Ok(&self.resources)
    }
    fn write(&self, bucket: &str) -> S3Result<&impl S3> {
        if bucket != S3_RESULTS_BUCKET {
            return Err(S3Error::new(S3ErrorCode::AccessDenied));
        }
        Ok(&self.results)
    }
}

#[async_trait::async_trait]
impl<Resources, Results> S3 for ProjectStorage<Resources, Results>
where
    Resources: S3,
    Results: S3,
{
    async fn head_bucket(&self, req: S3Request<HeadBucketInput>) -> S3Result<S3Response<HeadBucketOutput>> {
        match req.input.bucket.as_str() {
            S3_RESOURCES_BUCKET => self.resources.head_bucket(req).await,
            S3_RESULTS_BUCKET => self.results.head_bucket(req).await,
            _ => Err(S3Error::new(S3ErrorCode::AccessDenied)),
        }
    }
    async fn get_object(&self, req: S3Request<GetObjectInput>) -> S3Result<S3Response<GetObjectOutput>> {
        self.read(&req.input.bucket)?.get_object(req).await
    }
    async fn head_object(&self, req: S3Request<HeadObjectInput>) -> S3Result<S3Response<HeadObjectOutput>> {
        self.read(&req.input.bucket)?.head_object(req).await
    }
    async fn list_objects_v2(&self, req: S3Request<ListObjectsV2Input>) -> S3Result<S3Response<ListObjectsV2Output>> {
        self.read(&req.input.bucket)?.list_objects_v2(req).await
    }
    async fn put_object(&self, req: S3Request<PutObjectInput>) -> S3Result<S3Response<PutObjectOutput>> {
        self.write(&req.input.bucket)?.put_object(req).await
    }
    async fn create_multipart_upload(&self, req: S3Request<CreateMultipartUploadInput>) -> S3Result<S3Response<CreateMultipartUploadOutput>> {
        self.write(&req.input.bucket)?.create_multipart_upload(req).await
    }
    async fn upload_part(&self, req: S3Request<UploadPartInput>) -> S3Result<S3Response<UploadPartOutput>> {
        self.write(&req.input.bucket)?.upload_part(req).await
    }
    async fn complete_multipart_upload(&self, req: S3Request<CompleteMultipartUploadInput>) -> S3Result<S3Response<CompleteMultipartUploadOutput>> {
        self.write(&req.input.bucket)?.complete_multipart_upload(req).await
    }
    async fn abort_multipart_upload(&self, req: S3Request<AbortMultipartUploadInput>) -> S3Result<S3Response<AbortMultipartUploadOutput>> {
        self.write(&req.input.bucket)?.abort_multipart_upload(req).await
    }
}

#[cfg(test)]
mod tests;
