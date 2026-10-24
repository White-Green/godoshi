use super::*;
use futures::TryStreamExt;
use futures::executor::block_on;
use s3s::service::S3Service;
use std::sync::{Arc, Mutex};

struct RecordingS3 {
    bucket: &'static str,
    calls: Arc<Mutex<Vec<&'static str>>>,
}

impl RecordingS3 {
    fn record(&self, bucket: &str, operation: &'static str) {
        assert_eq!(bucket, self.bucket);
        self.calls.lock().unwrap().push(operation);
    }
}

#[async_trait::async_trait]
impl S3 for RecordingS3 {
    async fn head_bucket(&self, req: S3Request<HeadBucketInput>) -> S3Result<S3Response<HeadBucketOutput>> {
        self.record(&req.input.bucket, "head_bucket");
        Ok(S3Response::new(HeadBucketOutput::default()))
    }
    async fn get_object(&self, req: S3Request<GetObjectInput>) -> S3Result<S3Response<GetObjectOutput>> {
        self.record(&req.input.bucket, "get_object");
        if req.input.key == "missing" {
            return Err(S3Error::new(S3ErrorCode::NoSuchKey));
        }
        assert_eq!(req.input.key, "folder/file.txt");
        Ok(S3Response::new(GetObjectOutput {
            body: Some(bytes::Bytes::from_static(b"resource").into()),
            ..Default::default()
        }))
    }
    async fn head_object(&self, req: S3Request<HeadObjectInput>) -> S3Result<S3Response<HeadObjectOutput>> {
        self.record(&req.input.bucket, "head_object");
        assert_eq!(req.input.key, "folder/file.txt");
        Ok(S3Response::new(HeadObjectOutput::default()))
    }
    async fn list_objects_v2(&self, req: S3Request<ListObjectsV2Input>) -> S3Result<S3Response<ListObjectsV2Output>> {
        self.record(&req.input.bucket, "list_objects_v2");
        assert_eq!(req.input.prefix.as_deref(), Some("folder/"));
        Ok(S3Response::new(ListObjectsV2Output::default()))
    }
    async fn put_object(&self, req: S3Request<PutObjectInput>) -> S3Result<S3Response<PutObjectOutput>> {
        self.record(&req.input.bucket, "put_object");
        assert_eq!(req.input.key, "folder/file.txt");
        assert_eq!(
            req.input.body.unwrap().map_ok(|bytes| bytes.to_vec()).try_concat().await.unwrap(),
            b"output"[..]
        );
        Ok(S3Response::new(PutObjectOutput::default()))
    }
    async fn create_multipart_upload(&self, req: S3Request<CreateMultipartUploadInput>) -> S3Result<S3Response<CreateMultipartUploadOutput>> {
        self.record(&req.input.bucket, "create_multipart_upload");
        assert_eq!(req.input.key, "folder/file.txt");
        Ok(S3Response::new(CreateMultipartUploadOutput {
            upload_id: Some("upload-1".into()),
            ..Default::default()
        }))
    }
    async fn upload_part(&self, req: S3Request<UploadPartInput>) -> S3Result<S3Response<UploadPartOutput>> {
        self.record(&req.input.bucket, "upload_part");
        assert_eq!(req.input.key, "folder/file.txt");
        assert_eq!(req.input.upload_id, "upload-1");
        assert_eq!(req.input.part_number, 2);
        assert_eq!(
            req.input.body.unwrap().map_ok(|bytes| bytes.to_vec()).try_concat().await.unwrap(),
            b"part"[..]
        );
        Ok(S3Response::new(UploadPartOutput {
            e_tag: Some(ETag::Strong("etag-2".into())),
            ..Default::default()
        }))
    }
    async fn complete_multipart_upload(&self, req: S3Request<CompleteMultipartUploadInput>) -> S3Result<S3Response<CompleteMultipartUploadOutput>> {
        self.record(&req.input.bucket, "complete_multipart_upload");
        assert_eq!(req.input.key, "folder/file.txt");
        assert_eq!(req.input.upload_id, "upload-1");
        let parts = req.input.multipart_upload.unwrap().parts.unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].part_number, Some(2));
        assert_eq!(parts[0].e_tag, Some(ETag::Strong("etag-2".into())));
        Ok(S3Response::new(CompleteMultipartUploadOutput::default()))
    }
    async fn abort_multipart_upload(&self, req: S3Request<AbortMultipartUploadInput>) -> S3Result<S3Response<AbortMultipartUploadOutput>> {
        self.record(&req.input.bucket, "abort_multipart_upload");
        assert_eq!(req.input.key, "folder/file.txt");
        assert_eq!(req.input.upload_id, "upload-1");
        Ok(S3Response::new(AbortMultipartUploadOutput::default()))
    }
}

fn service() -> (S3Service, Arc<Mutex<Vec<&'static str>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let resources = RecordingS3 {
        bucket: S3_RESOURCES_BUCKET,
        calls: calls.clone(),
    };
    let results = RecordingS3 {
        bucket: S3_RESULTS_BUCKET,
        calls: calls.clone(),
    };
    (S3ServiceBuilder::new(ProjectStorage { resources, results }).build(), calls)
}

fn request(method: &str, path: &str, body: &str) -> s3s::HttpRequest {
    axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost")
        .header("content-length", body.len())
        .body(s3s::Body::from(body.to_owned()))
        .unwrap()
}

#[test]
fn multipart_http_operations_use_result_port() {
    block_on(async {
        let (service, calls) = service();
        let mut created = service.call(request("POST", "/results/folder/file.txt?uploads", "")).await.unwrap();
        assert_eq!(created.status(), 200);
        assert!(
            String::from_utf8(created.body_mut().store_all_limited(4096).await.unwrap().to_vec())
                .unwrap()
                .contains("<UploadId>upload-1</UploadId>")
        );
        let uploaded = service
            .call(request("PUT", "/results/folder/file.txt?uploadId=upload-1&partNumber=2", "part"))
            .await
            .unwrap();
        assert_eq!(uploaded.status(), 200);
        assert_eq!(uploaded.headers()["etag"], "\"etag-2\"");
        let manifest = "<CompleteMultipartUpload><Part><PartNumber>2</PartNumber><ETag>\"etag-2\"</ETag></Part></CompleteMultipartUpload>";
        assert_eq!(
            service
                .call(request("POST", "/results/folder/file.txt?uploadId=upload-1", manifest))
                .await
                .unwrap()
                .status(),
            200
        );
        assert_eq!(
            service
                .call(request("DELETE", "/results/folder/file.txt?uploadId=upload-1", ""))
                .await
                .unwrap()
                .status(),
            204
        );
        assert_eq!(
            *calls.lock().unwrap(),
            [
                "create_multipart_upload",
                "upload_part",
                "complete_multipart_upload",
                "abort_multipart_upload"
            ]
        );
    });
}

#[test]
fn object_http_operations_use_scoped_ports() {
    block_on(async {
        let (service, calls) = service();
        for (method, path, body) in [
            ("HEAD", "/resources", ""),
            ("HEAD", "/results", ""),
            ("GET", "/resources/folder/file.txt", ""),
            ("HEAD", "/resources/folder/file.txt", ""),
            ("GET", "/resources?list-type=2&prefix=folder/", ""),
            ("PUT", "/results/folder/file.txt", "output"),
        ] {
            assert_eq!(service.call(request(method, path, body)).await.unwrap().status(), 200, "{method} {path}");
        }
        assert_eq!(
            *calls.lock().unwrap(),
            ["head_bucket", "head_bucket", "get_object", "head_object", "list_objects_v2", "put_object"]
        );
    });
}

#[test]
fn disallowed_operations_never_reach_ports() {
    block_on(async {
        let (service, calls) = service();
        for (method, path, body) in [
            ("HEAD", "/other", ""),
            ("GET", "/results/file", ""),
            ("HEAD", "/results/file", ""),
            ("GET", "/results?list-type=2", ""),
            ("PUT", "/resources/file", "output"),
            ("POST", "/resources/file?uploads", ""),
            ("PUT", "/resources/file?uploadId=upload-1&partNumber=2", "part"),
            ("POST", "/resources/file?uploadId=upload-1", "<CompleteMultipartUpload/>"),
            ("DELETE", "/resources/file?uploadId=upload-1", ""),
        ] {
            assert_eq!(service.call(request(method, path, body)).await.unwrap().status(), 403, "{method} {path}");
        }
        assert!(calls.lock().unwrap().is_empty());
    });
}

#[test]
fn backend_errors_preserve_s3_codes() {
    block_on(async {
        let (service, _) = service();
        let mut response = service.call(request("GET", "/resources/missing", "")).await.unwrap();
        assert_eq!(response.status(), 404);
        let body = response.body_mut().store_all_limited(4096).await.unwrap();
        assert!(String::from_utf8(body.to_vec()).unwrap().contains("<Code>NoSuchKey</Code>"));
    });
}

#[test]
fn invalid_run_id_is_rejected_before_opening_storage() {
    block_on(async {
        let bucket = crate::test_support::MemoryBucket::default();
        for run_id in ["", "../other", "run/other", "run", "abc-123", "998232709354", "1000000000000-1"] {
            let result = handle(
                request("POST", "/results/file?uploads", ""),
                &bucket,
                Params {
                    project_id: ProjectId::new(1),
                    run_id: run_id.into(),
                },
            )
            .await;
            assert_eq!(result.unwrap_err().code(), &S3ErrorCode::InvalidArgument);
        }
    });
}
