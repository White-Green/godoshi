use axum::http;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use checksum::{Algorithm, Kind, Multipart, Part, UploadChecksums};
use futures::FutureExt;
use futures::future::BoxFuture;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use s3s::dto::{
    AbortMultipartUploadInput, AbortMultipartUploadOutput, CommonPrefix, CompleteMultipartUploadInput, CompleteMultipartUploadOutput,
    CreateMultipartUploadInput, CreateMultipartUploadOutput, ETag, ETagCondition, GetObjectInput, GetObjectOutput, HeadBucketInput, HeadBucketOutput,
    HeadObjectInput, HeadObjectOutput, ListObjectsV2Input, ListObjectsV2Output, Object, ObjectStorageClass, PutObjectInput, PutObjectOutput,
    StreamingBlob, Timestamp, TimestampFormat, UploadPartInput, UploadPartOutput,
};
use s3s::service::S3ServiceBuilder;
use s3s::{S3, S3Error, S3ErrorCode, S3Request, S3Response, S3Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use worker::send::{SendFuture, SendWrapper};
use worker::{Bucket, Conditional, D1Database, Data, HttpMetadata, Request};

mod checksum;
mod multipart;

#[derive(Deserialize, Serialize)]
pub struct Params {
    pub project_id: u64,
}

struct ProjectStorage {
    r2: SendWrapper<Bucket>,
    db: D1Database,
    project_id: u64,
}

pub async fn handle(req: Request, r2: Bucket, db: D1Database, params: Params) -> S3Result<s3s::HttpResponse> {
    let request = http::Request::try_from(req)
        .map_err(|error| {
            tracing::error!(%error, "S3 request conversion failed");
            S3Error::new(S3ErrorCode::InternalError)
        })?
        .map(s3s::Body::http_body);
    S3ServiceBuilder::new(ProjectStorage {
        r2: SendWrapper::new(r2),
        db,
        project_id: params.project_id,
    })
    .build()
    .call(request)
    .await
    .map_err(|error| {
        tracing::error!(?error, "S3 protocol handler failed");
        S3Error::with_source(S3ErrorCode::InternalError, error.into_std_error())
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessType {
    Read,
    Write,
}

impl ProjectStorage {
    fn key(&self, bucket: &str, key: &str, access_type: AccessType) -> S3Result<String> {
        let access_allowed = match access_type {
            AccessType::Read => bucket == "resources",
            AccessType::Write => bucket == "result",
        };
        if !access_allowed {
            return Err(S3Error::new(S3ErrorCode::AccessDenied));
        }
        validate_key(key)?;
        let internal_key = format!("projects/{}/{bucket}/{key}", self.project_id);
        if internal_key.len() > 1024 {
            return Err(S3Error::new(S3ErrorCode::InvalidArgument));
        }
        Ok(internal_key)
    }
}

macro_rules! reject_unsupported {
    ($input:expr, $($field:ident),+ $(,)?) => {
        if $($input.$field.is_some())||+ {
            return Err(S3Error::new(S3ErrorCode::NotImplemented));
        }
    };
}

#[async_trait::async_trait]
impl S3 for ProjectStorage {
    fn abort_multipart_upload<'l0, 'async_trait>(
        &'l0 self,
        req: S3Request<AbortMultipartUploadInput>,
    ) -> BoxFuture<'async_trait, S3Result<S3Response<AbortMultipartUploadOutput>>>
    where
        Self: 'async_trait,
        'l0: 'async_trait,
    {
        SendFuture::new(async move {
            let AbortMultipartUploadInput { bucket, key, upload_id, .. } = req.input;
            let internal_key = self.key(&bucket, &key, AccessType::Write)?;
            self.r2
                .resume_multipart_upload(&internal_key, &upload_id)
                .map_err(r2_error)?
                .abort()
                .await
                .map_err(r2_error)?;
            multipart::remove(&self.db, &internal_key, &upload_id).await;
            Ok(S3Response::new(AbortMultipartUploadOutput::default()))
        })
        .boxed()
    }

    fn complete_multipart_upload<'l0, 'async_trait>(
        &'l0 self,
        req: S3Request<CompleteMultipartUploadInput>,
    ) -> BoxFuture<'async_trait, S3Result<S3Response<CompleteMultipartUploadOutput>>>
    where
        Self: 'async_trait,
        'l0: 'async_trait,
    {
        SendFuture::new(async move {
            reject_unsupported!(
                req.input,
                if_match,
                if_none_match,
                sse_customer_algorithm,
                sse_customer_key,
                sse_customer_key_md5,
                checksum_crc32c,
                checksum_md5,
                checksum_sha1,
                checksum_sha256,
                checksum_sha512,
                checksum_xxhash64,
                checksum_xxhash3,
                checksum_xxhash128,
            );
            let CompleteMultipartUploadInput {
                bucket,
                key,
                upload_id,
                multipart_upload,
                checksum_crc32,
                checksum_crc64nvme,
                checksum_type,
                mpu_object_size,
                ..
            } = req.input;
            let internal_key = self.key(&bucket, &key, AccessType::Write)?;
            let upload = multipart::get(&self.db, &internal_key, &upload_id).await?;
            let mut algorithm = upload.algorithm();
            if checksum_type.is_some_and(|kind| kind.as_str() != algorithm.multipart_type().name()) {
                return Err(S3Error::new(S3ErrorCode::BadDigest));
            }
            let manifest = multipart_upload
                .and_then(|upload| upload.parts)
                .ok_or_else(|| S3Error::new(S3ErrorCode::MalformedXML))?;
            if manifest.is_empty() || manifest.len() > 10000 {
                return Err(S3Error::new(S3ErrorCode::InvalidArgument));
            }
            let mut saved_parts = multipart::parts(&self.db, &internal_key, &upload_id).await?;
            let mut previous = 0;
            let mut parts = Vec::with_capacity(manifest.len());
            let mut checksums = Vec::with_capacity(manifest.len());
            for part in manifest {
                let number = part_number(part.part_number.ok_or_else(|| S3Error::new(S3ErrorCode::InvalidPart))?)?;
                if number <= previous {
                    return Err(S3Error::new(S3ErrorCode::InvalidPartOrder));
                }
                let etag = part
                    .e_tag
                    .and_then(ETag::into_strong)
                    .filter(|etag| !etag.is_empty())
                    .ok_or_else(|| S3Error::new(S3ErrorCode::InvalidPart))?;
                let saved = saved_parts
                    .remove(&(number, etag.clone()))
                    .ok_or_else(|| S3Error::new(S3ErrorCode::InvalidPart))?;
                if previous == 0 && upload.algorithm.is_none() {
                    algorithm = saved.sum.algorithm;
                }
                let supplied = match algorithm {
                    Algorithm::Crc32 => part.checksum_crc32,
                    Algorithm::Crc64nvme => part.checksum_crc64nvme,
                };
                if saved.sum.algorithm != algorithm || supplied.as_ref().is_some_and(|s| *s != saved.sum.value) {
                    return Err(S3Error::new(S3ErrorCode::BadDigest));
                }
                if upload.algorithm.is_some() && algorithm.multipart_type() == Kind::Composite && (number != previous + 1 || supplied.is_none()) {
                    return Err(S3Error::new(S3ErrorCode::InvalidPart));
                }
                previous = number;
                parts.push(worker::UploadedPart::new(number, etag));
                checksums.push(saved);
            }
            let sum = algorithm.combine(&checksums)?;
            if let Some((algorithm, value)) = checksum::request_sum(checksum_crc32, checksum_crc64nvme)?
                && (algorithm != sum.algorithm || value != sum.value)
            {
                return Err(S3Error::new(S3ErrorCode::BadDigest));
            }
            if let Some(size) = mpu_object_size
                && u64::try_from(size).ok() != Some(checksums.iter().map(|p| p.length).sum())
            {
                return Err(S3Error::new(S3ErrorCode::InvalidRequest));
            }
            let result = self
                .r2
                .resume_multipart_upload(&internal_key, &upload_id)
                .map_err(r2_error)?
                .complete(parts)
                .await
                .map_err(r2_error)?;
            multipart::remove(&self.db, &internal_key, &upload_id).await;
            let checksum = sum.into_checksum();
            Ok(S3Response::new(CompleteMultipartUploadOutput {
                bucket: Some(bucket),
                key: Some(key),
                e_tag: Some(ETag::Strong(result.etag())),
                checksum_crc32: checksum.checksum_crc32,
                checksum_crc64nvme: checksum.checksum_crc64nvme,
                checksum_type: checksum.checksum_type,
                ..Default::default()
            }))
        })
        .boxed()
    }

    fn create_multipart_upload<'l0, 'async_trait>(
        &'l0 self,
        req: S3Request<CreateMultipartUploadInput>,
    ) -> BoxFuture<'async_trait, S3Result<S3Response<CreateMultipartUploadOutput>>>
    where
        Self: 'async_trait,
        'l0: 'async_trait,
    {
        SendFuture::new(async move {
            reject_unsupported!(
                req.input,
                acl,
                grant_full_control,
                grant_read,
                grant_read_acp,
                grant_write_acp,
                server_side_encryption,
                bucket_key_enabled,
                sse_customer_algorithm,
                sse_customer_key,
                sse_customer_key_md5,
                ssekms_encryption_context,
                ssekms_key_id,
                object_lock_legal_hold_status,
                object_lock_mode,
                object_lock_retain_until_date,
                tagging,
            );
            let CreateMultipartUploadInput {
                bucket,
                key,
                content_type,
                content_language,
                content_disposition,
                content_encoding,
                cache_control,
                expires,
                metadata,
                checksum_algorithm,
                checksum_type,
                ..
            } = req.input;
            let internal_key = self.key(&bucket, &key, AccessType::Write)?;
            let checksum = Multipart::new(checksum_algorithm, checksum_type)?;
            let result = self
                .r2
                .create_multipart_upload(&internal_key)
                .http_metadata(HttpMetadata {
                    content_type,
                    content_language,
                    content_disposition,
                    content_encoding: checksum::encoding(content_encoding),
                    cache_control,
                    cache_expiry: expires.map(r2_date).transpose()?,
                })
                .custom_metadata(metadata.unwrap_or_default().into_iter().collect::<HashMap<_, _>>())
                .execute()
                .await
                .map_err(r2_error)?;
            let upload_id = result.upload_id().await;
            if let Err(error) = multipart::create(&self.db, &internal_key, &upload_id, &checksum).await {
                if let Err(abort_error) = result.abort().await {
                    tracing::warn!(%abort_error, "Could not abort R2 multipart upload after database failure");
                }
                return Err(error);
            }
            Ok(S3Response::new(CreateMultipartUploadOutput {
                bucket: Some(bucket),
                key: Some(key),
                upload_id: Some(upload_id),
                checksum_algorithm: Some(checksum.algorithm().name().to_owned().into()),
                checksum_type: Some(checksum.algorithm().multipart_type().into()),
                ..Default::default()
            }))
        })
        .boxed()
    }

    fn get_object<'l0, 'async_trait>(&'l0 self, req: S3Request<GetObjectInput>) -> BoxFuture<'async_trait, S3Result<S3Response<GetObjectOutput>>>
    where
        Self: 'async_trait,
        'l0: 'async_trait,
    {
        SendFuture::new(async move {
            let GetObjectInput {
                bucket,
                checksum_mode,
                expected_bucket_owner: _,
                if_match,
                if_modified_since,
                if_none_match,
                if_unmodified_since,
                key,
                part_number,
                range,
                request_payer: _,
                response_cache_control: _,
                response_content_disposition: _,
                response_content_encoding: _,
                response_content_language: _,
                response_content_type: _,
                response_expires: _,
                sse_customer_algorithm,
                sse_customer_key,
                sse_customer_key_md5,
                version_id,
            } = req.input;
            let internal_key = self.key(&bucket, &key, AccessType::Read)?;
            if version_id.is_some()
                || part_number.is_some()
                || sse_customer_algorithm.is_some()
                || sse_customer_key.is_some()
                || sse_customer_key_md5.is_some()
                || if_modified_since.is_some()
                || if_unmodified_since.is_some()
            {
                return Err(S3Error::new(S3ErrorCode::NotImplemented));
            }
            let mut get_options = self.r2.get(internal_key);
            if let Some(range) = range {
                get_options = get_options.range(r2_range(range)?);
            }
            if if_match.is_some() || if_none_match.is_some() {
                get_options = get_options.only_if(Conditional {
                    etag_matches: if_match
                        .as_ref()
                        .and_then(ETagCondition::as_etag)
                        .and_then(ETag::as_strong)
                        .map(str::to_owned),
                    etag_does_not_match: if_none_match
                        .as_ref()
                        .and_then(ETagCondition::as_etag)
                        .map(|etag| etag.value().to_owned()),
                    uploaded_before: None,
                    uploaded_after: None,
                });
            }
            let result = get_options
                .execute()
                .await
                .map_err(r2_error)?
                .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchKey))?;
            check_read_conditions(&result, if_match.as_ref(), if_none_match.as_ref())?;
            let size = result.size();
            let checksum = if checksum_mode.is_some() && range.is_none() {
                checksum::r2_checksum(&result)
            } else {
                Default::default()
            };
            let HttpMetadata {
                content_type,
                content_language,
                content_disposition,
                content_encoding,
                cache_control,
                cache_expiry,
            } = result.http_metadata();
            let (content_length, content_range) = if let Some(range) = range {
                let returned_range = checked_range(range, size)?;
                (
                    (returned_range.end - returned_range.start) as i64,
                    Some(format!("bytes {}-{}/{size}", returned_range.start, returned_range.end - 1)),
                )
            } else {
                (size as i64, None)
            };
            let body = result
                .body()
                .ok_or_else(|| S3Error::new(S3ErrorCode::InternalError))?
                .response_body()
                .map_err(r2_error)?;
            let worker::ResponseBody::Stream(stream) = body else {
                return Err(S3Error::new(S3ErrorCode::InternalError));
            };
            Ok(S3Response::new(GetObjectOutput {
                accept_ranges: Some("bytes".into()),
                body: Some(StreamingBlob::wrap(worker::Body::new(stream))),
                cache_control,
                content_disposition,
                content_encoding,
                content_language,
                content_length: Some(content_length),
                content_range,
                content_type,
                e_tag: Some(ETag::Strong(result.etag())),
                expires: cache_expiry.map(http_date).transpose()?,
                last_modified: Some(timestamp(result.uploaded())?),
                metadata: Some(result.custom_metadata().map_err(r2_error)?.into_iter().collect()),
                checksum_sha256: checksum.checksum_sha256,
                checksum_sha1: checksum.checksum_sha1,
                checksum_type: checksum.checksum_type,
                ..Default::default()
            }))
        })
        .boxed()
    }

    async fn head_bucket(&self, req: S3Request<HeadBucketInput>) -> S3Result<S3Response<HeadBucketOutput>> {
        let HeadBucketInput { bucket, .. } = req.input;
        if !matches!(bucket.as_str(), "resources" | "result") {
            return Err(S3Error::new(S3ErrorCode::AccessDenied));
        }
        Ok(S3Response::new(HeadBucketOutput::default()))
    }

    fn head_object<'l0, 'async_trait>(&'l0 self, req: S3Request<HeadObjectInput>) -> BoxFuture<'async_trait, S3Result<S3Response<HeadObjectOutput>>>
    where
        Self: 'async_trait,
        'l0: 'async_trait,
    {
        SendFuture::new(async move {
            let HeadObjectInput {
                bucket,
                key,
                range,
                if_match,
                if_none_match,
                if_modified_since,
                if_unmodified_since,
                version_id,
                part_number,
                checksum_mode,
                sse_customer_algorithm,
                sse_customer_key,
                sse_customer_key_md5,
                ..
            } = req.input;
            let internal_key = self.key(&bucket, &key, AccessType::Read)?;
            if version_id.is_some()
                || part_number.is_some()
                || sse_customer_algorithm.is_some()
                || sse_customer_key.is_some()
                || sse_customer_key_md5.is_some()
                || if_modified_since.is_some()
                || if_unmodified_since.is_some()
            {
                return Err(S3Error::new(S3ErrorCode::NotImplemented));
            }
            let result = self
                .r2
                .head(internal_key)
                .await
                .map_err(r2_error)?
                .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchKey))?;
            check_read_conditions(&result, if_match.as_ref(), if_none_match.as_ref())?;
            let size = result.size();
            let checksum = if checksum_mode.is_some() && range.is_none() {
                checksum::r2_checksum(&result)
            } else {
                Default::default()
            };
            let HttpMetadata {
                content_type,
                content_language,
                content_disposition,
                content_encoding,
                cache_control,
                cache_expiry,
            } = result.http_metadata();
            let (content_length, content_range) = if let Some(range) = range {
                let range = checked_range(range, size)?;
                (
                    (range.end - range.start) as i64,
                    Some(format!("bytes {}-{}/{size}", range.start, range.end - 1)),
                )
            } else {
                (size as i64, None)
            };
            Ok(S3Response::new(HeadObjectOutput {
                accept_ranges: Some("bytes".into()),
                cache_control,
                content_disposition,
                content_encoding,
                content_language,
                content_length: Some(content_length),
                content_range,
                content_type,
                e_tag: Some(ETag::Strong(result.etag())),
                expires: cache_expiry.map(http_date).transpose()?,
                last_modified: Some(timestamp(result.uploaded())?),
                metadata: Some(result.custom_metadata().map_err(r2_error)?.into_iter().collect()),
                checksum_sha256: checksum.checksum_sha256,
                checksum_sha1: checksum.checksum_sha1,
                checksum_type: checksum.checksum_type,
                ..Default::default()
            }))
        })
        .boxed()
    }

    fn list_objects_v2<'l0, 'async_trait>(
        &'l0 self,
        req: S3Request<ListObjectsV2Input>,
    ) -> BoxFuture<'async_trait, S3Result<S3Response<ListObjectsV2Output>>>
    where
        Self: 'async_trait,
        'l0: 'async_trait,
    {
        SendFuture::new(async move {
            let ListObjectsV2Input {
                bucket,
                prefix,
                max_keys,
                encoding_type,
                delimiter,
                continuation_token,
                start_after,
                ..
            } = req.input;
            let scope = self.key(&bucket, "", AccessType::Read)?;
            let prefix = prefix.unwrap_or_default();
            validate_key(&prefix)?;
            let limit = max_keys.unwrap_or(1000);
            if limit < 0 || encoding_type.as_ref().is_some_and(|value| value.as_str() != "url") {
                return Err(S3Error::new(S3ErrorCode::InvalidArgument));
            }
            let limit = limit.min(1000);
            let encode = |value: &str| {
                if encoding_type.is_some() {
                    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
                } else {
                    value.to_owned()
                }
            };
            let mut list_options = self.r2.list().prefix(format!("{scope}{prefix}")).limit(limit.max(1) as u32);
            if let Some(delimiter) = delimiter.as_ref().filter(|value| !value.is_empty()) {
                list_options = list_options.delimiter(delimiter);
            }
            if let Some(cursor) = &continuation_token {
                list_options = list_options.cursor(cursor);
            } else if let Some(start) = &start_after {
                validate_key(start)?;
                list_options = list_options.start_after(format!("{scope}{start}"));
            }
            let mut output = ListObjectsV2Output {
                name: Some(bucket),
                prefix: Some(encode(&prefix)),
                max_keys: Some(limit),
                delimiter: delimiter.as_deref().map(encode),
                start_after: start_after.as_deref().map(encode),
                encoding_type: encoding_type.clone(),
                continuation_token,
                key_count: Some(0),
                is_truncated: Some(false),
                ..Default::default()
            };
            if limit > 0 {
                let result = list_options.execute().await.map_err(r2_error)?;
                let mut contents = Vec::new();
                for object in result.objects() {
                    let key = object.key();
                    let key = key.strip_prefix(&scope).ok_or_else(|| S3Error::new(S3ErrorCode::InternalError))?;
                    contents.push(Object {
                        key: Some(encode(key)),
                        size: Some(object.size() as i64),
                        e_tag: Some(ETag::Strong(object.etag())),
                        last_modified: Some(timestamp(object.uploaded())?),
                        storage_class: Some(ObjectStorageClass::from_static(ObjectStorageClass::STANDARD)),
                        ..Default::default()
                    });
                }
                let mut prefixes = Vec::new();
                for prefix in result.delimited_prefixes() {
                    let prefix = prefix.strip_prefix(&scope).ok_or_else(|| S3Error::new(S3ErrorCode::InternalError))?;
                    prefixes.push(CommonPrefix {
                        prefix: Some(encode(prefix)),
                    });
                }
                output.key_count = Some((contents.len() + prefixes.len()) as i32);
                output.contents = Some(contents);
                output.common_prefixes = Some(prefixes);
                output.is_truncated = Some(result.truncated());
                output.next_continuation_token = result.cursor().filter(|_| result.truncated());
            }
            Ok(S3Response::new(output))
        })
        .boxed()
    }

    fn put_object<'l0, 'async_trait>(&'l0 self, req: S3Request<PutObjectInput>) -> BoxFuture<'async_trait, S3Result<S3Response<PutObjectOutput>>>
    where
        Self: 'async_trait,
        'l0: 'async_trait,
    {
        SendFuture::new(async move {
            reject_unsupported!(
                req.input,
                acl,
                grant_full_control,
                grant_read,
                grant_read_acp,
                grant_write_acp,
                server_side_encryption,
                bucket_key_enabled,
                sse_customer_algorithm,
                sse_customer_key,
                sse_customer_key_md5,
                ssekms_encryption_context,
                ssekms_key_id,
                object_lock_legal_hold_status,
                object_lock_mode,
                object_lock_retain_until_date,
                tagging,
                if_match,
                if_none_match,
                write_offset_bytes,
                checksum_crc32c,
                checksum_md5,
                checksum_sha1,
                checksum_sha256,
                checksum_sha512,
                checksum_xxhash64,
                checksum_xxhash3,
                checksum_xxhash128,
            );
            let PutObjectInput {
                bucket,
                key,
                body,
                content_length,
                content_md5,
                checksum_algorithm,
                checksum_crc32,
                checksum_crc64nvme,
                content_type,
                content_language,
                content_disposition,
                content_encoding,
                cache_control,
                expires,
                metadata,
                ..
            } = req.input;
            let internal_key = self.key(&bucket, &key, AccessType::Write)?;
            let (data, verification) = checksum::upload(
                body,
                content_length,
                content_encoding.as_deref(),
                &req.headers,
                UploadChecksums {
                    algorithm: checksum_algorithm,
                    crc32: checksum_crc32,
                    crc64nvme: checksum_crc64nvme,
                    md5: content_md5.clone(),
                },
                None,
            )
            .await?;
            let mut put_options = self
                .r2
                .put(internal_key, data)
                .http_metadata(HttpMetadata {
                    content_type,
                    content_language,
                    content_disposition,
                    content_encoding: checksum::encoding(content_encoding),
                    cache_control,
                    cache_expiry: expires.map(r2_date).transpose()?,
                })
                .custom_metadata(metadata.unwrap_or_default().into_iter().collect::<HashMap<_, _>>());
            if let Some(md5) = content_md5 {
                let digest = STANDARD.decode(md5).map_err(|_| S3Error::new(S3ErrorCode::InvalidDigest))?;
                if digest.len() != 16 {
                    return Err(S3Error::new(S3ErrorCode::InvalidDigest));
                }
                put_options = put_options.md5(digest);
            }
            let result = put_options
                .execute()
                .await
                .map_err(|e| verification.error(e))?
                .ok_or_else(|| S3Error::new(S3ErrorCode::PreconditionFailed))?;
            let checksum = verification.finish()?.into_checksum();
            Ok(S3Response::new(PutObjectOutput {
                e_tag: Some(ETag::Strong(result.etag())),
                checksum_crc32: checksum.checksum_crc32,
                checksum_crc64nvme: checksum.checksum_crc64nvme,
                checksum_type: checksum.checksum_type,
                ..Default::default()
            }))
        })
        .boxed()
    }

    fn upload_part<'l0, 'async_trait>(&'l0 self, req: S3Request<UploadPartInput>) -> BoxFuture<'async_trait, S3Result<S3Response<UploadPartOutput>>>
    where
        Self: 'async_trait,
        'l0: 'async_trait,
    {
        SendFuture::new(async move {
            reject_unsupported!(
                req.input,
                sse_customer_algorithm,
                sse_customer_key,
                sse_customer_key_md5,
                checksum_crc32c,
                checksum_md5,
                checksum_sha1,
                checksum_sha256,
                checksum_sha512,
                checksum_xxhash64,
                checksum_xxhash3,
                checksum_xxhash128,
            );
            let UploadPartInput {
                bucket,
                key,
                upload_id,
                part_number: number,
                body,
                content_length,
                content_md5,
                checksum_algorithm,
                checksum_crc32,
                checksum_crc64nvme,
                ..
            } = req.input;
            let internal_key = self.key(&bucket, &key, AccessType::Write)?;
            let number = part_number(number)?;
            let upload = multipart::get(&self.db, &internal_key, &upload_id).await?;
            let (data, verification) = checksum::upload(
                body,
                content_length,
                checksum::header(&req.headers, "content-encoding")?,
                &req.headers,
                UploadChecksums {
                    algorithm: checksum_algorithm,
                    crc32: checksum_crc32,
                    crc64nvme: checksum_crc64nvme,
                    md5: content_md5,
                },
                upload.algorithm,
            )
            .await?;
            let result = self
                .r2
                .resume_multipart_upload(&internal_key, &upload_id)
                .map_err(r2_error)?
                .upload_part(number, data)
                .await
                .map_err(|e| verification.error(e))?;
            let sum = verification.finish()?;
            let etag = result.etag().trim_matches('"').to_owned();
            let part = Part {
                length: verification.length,
                sum: sum.clone(),
            };
            multipart::save_part(&self.db, &internal_key, &upload_id, number, &etag, &part).await?;
            let checksum = sum.into_checksum();
            Ok(S3Response::new(UploadPartOutput {
                e_tag: Some(ETag::Strong(etag)),
                checksum_crc32: checksum.checksum_crc32,
                checksum_crc64nvme: checksum.checksum_crc64nvme,
                ..Default::default()
            }))
        })
        .boxed()
    }
}

fn validate_key(key: &str) -> S3Result<()> {
    if key.len() > 1024 || key.chars().any(char::is_control) || key.contains('\\') || key.split('/').any(|part| matches!(part, "." | "..")) {
        return Err(S3Error::new(S3ErrorCode::InvalidArgument));
    }
    Ok(())
}

fn r2_range(range: s3s::dto::Range) -> S3Result<worker::Range> {
    const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
    match range {
        s3s::dto::Range::Int { first, last } if first <= MAX_SAFE_INTEGER => match last {
            Some(last) => Ok(worker::Range::OffsetWithLength {
                offset: first,
                length: (last - first + 1).min(MAX_SAFE_INTEGER),
            }),
            None => Ok(worker::Range::OffsetToEnd { offset: first }),
        },
        s3s::dto::Range::Suffix { length } if length > 0 => Ok(worker::Range::Suffix {
            suffix: length.min(MAX_SAFE_INTEGER),
        }),
        _ => Err(S3Error::new(S3ErrorCode::InvalidRange)),
    }
}

fn checked_range(range: s3s::dto::Range, size: u64) -> S3Result<std::ops::Range<u64>> {
    let range = range.check(size)?;
    if range.is_empty() {
        return Err(S3Error::new(S3ErrorCode::InvalidRange));
    }
    Ok(range)
}

fn check_read_conditions(object: &worker::Object, if_match: Option<&ETagCondition>, if_none_match: Option<&ETagCondition>) -> S3Result<()> {
    let etag = ETag::Strong(object.etag());
    if if_match.is_some_and(|condition| !etag_matches(condition, &etag, ETag::strong_cmp)) {
        return Err(S3Error::new(S3ErrorCode::PreconditionFailed));
    }
    if if_none_match.is_some_and(|condition| etag_matches(condition, &etag, ETag::weak_cmp)) {
        return Err(S3Error::new(S3ErrorCode::NotModified));
    }
    Ok(())
}

fn etag_matches(condition: &ETagCondition, etag: &ETag, compare: fn(&ETag, &ETag) -> bool) -> bool {
    condition.is_any() || condition.as_etag().is_some_and(|value| compare(value, etag))
}

fn timestamp(date: worker::Date) -> S3Result<Timestamp> {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(date.as_millis()) * 1_000_000)
        .map(Into::into)
        .map_err(|_| S3Error::new(S3ErrorCode::InternalError))
}

fn http_date(date: worker::Date) -> S3Result<String> {
    let mut bytes = Vec::new();
    timestamp(date)?
        .format(TimestampFormat::HttpDate, &mut bytes)
        .map_err(|_| S3Error::new(S3ErrorCode::InternalError))?;
    String::from_utf8(bytes).map_err(|_| S3Error::new(S3ErrorCode::InternalError))
}

fn r2_date(date: String) -> S3Result<worker::Date> {
    Timestamp::parse(TimestampFormat::HttpDate, &date).map_err(|_| S3Error::new(S3ErrorCode::InvalidArgument))?;
    Ok(worker::DateInit::String(date).into())
}

fn part_number(number: i32) -> S3Result<u16> {
    if !(1..=10000).contains(&number) {
        return Err(S3Error::new(S3ErrorCode::InvalidArgument));
    }
    Ok(number as u16)
}

// Public R2 numeric-code/S3-code mappings and the documented message suffix:
// https://developers.cloudflare.com/r2/api/error-codes/#how-errors-are-returned
// https://developers.cloudflare.com/r2/api/error-codes/#error-code-reference
fn r2_error(error: worker::Error) -> S3Error {
    let message = error.to_string();
    // The Workers API appends "(<code>)" to the message; avoid matching error prose.
    let code = message.rsplit_once('(').and_then(|(_, code)| code.strip_suffix(')'));
    let code = match code {
        Some("10007") => S3ErrorCode::NoSuchKey,
        Some("10011") => S3ErrorCode::EntityTooSmall,
        Some("10012") => S3ErrorCode::MetadataTooLarge,
        // 10022 (INVALID_MAX_KEYS) and 10029 (INVALID_ARGUMENT) are not listed in
        // the public table; see R2ErrorCode in Miniflare (checked in 5.20260926.1-alpha):
        // https://github.com/cloudflare/workers-sdk/blob/main/packages/miniflare/src/workers/r2/errors.worker.ts
        // This handler normalizes these and 10020 (InvalidObjectName) to InvalidArgument.
        Some("10020" | "10022" | "10029") => S3ErrorCode::InvalidArgument,
        Some("10024") => S3ErrorCode::NoSuchUpload,
        Some("10025") => S3ErrorCode::InvalidPart,
        Some("10031") => S3ErrorCode::PreconditionFailed,
        Some("10037") => S3ErrorCode::BadDigest,
        Some("10039") => S3ErrorCode::InvalidRange,
        Some("100100") => S3ErrorCode::EntityTooLarge,
        _ => {
            // Handler policy: unknown codes or message formats are backend failures.
            tracing::error!(%error, "S3 binding operation failed");
            S3ErrorCode::InternalError
        }
    };
    S3Error::new(code)
}
