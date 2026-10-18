use super::{R2Scope, error};
use crate::core::bucket::BucketError;
use bytes::Bytes;
use futures::{Stream, TryStreamExt};

// Buffer at most one part. Streams ending within this size use a single PUT;
// larger streams use equal-sized multipart parts (apart from the final part).
const PART_SIZE: usize = 16 * 1024 * 1024;

#[worker::send]
pub(super) async fn put(scope: &R2Scope<'_>, key: &str, data: impl Stream<Item = Result<Bytes, std::io::Error>>) -> Result<(), BucketError> {
    let mut data = std::pin::pin!(data);
    let key = scope.key(key)?;
    let mut upload = None;
    let result = async {
        let mut parts = Vec::new();
        let mut buffer = Vec::with_capacity(PART_SIZE);
        while let Some(mut chunk) = data.try_next().await.map_err(BucketError::Read)? {
            while !chunk.is_empty() {
                // Wait for more data before switching so exactly PART_SIZE bytes
                // (including streams followed by empty chunks) still use one PUT.
                if buffer.len() == PART_SIZE {
                    if upload.is_none() {
                        upload = Some(
                            scope
                                .bucket
                                .create_multipart_upload(&key)
                                .http_metadata(metadata())
                                .execute()
                                .await
                                .map_err(error)?,
                        );
                    }
                    upload_part(upload.as_ref().unwrap(), &mut parts, std::mem::take(&mut buffer)).await?;
                    buffer = Vec::with_capacity(PART_SIZE);
                }
                let count = (PART_SIZE - buffer.len()).min(chunk.len());
                buffer.extend_from_slice(&chunk[..count]);
                chunk = chunk.slice(count..);
            }
        }
        if let Some(upload) = &upload {
            if !buffer.is_empty() {
                upload_part(upload, &mut parts, buffer).await?;
            }
            // Complete consumes its handle; retain the original for error cleanup.
            scope
                .bucket
                .resume_multipart_upload(&key, &upload.upload_id().await)
                .map_err(error)?
                .complete(parts)
                .await
                .map_err(error)?;
        } else {
            scope.bucket.put(&key, buffer).http_metadata(metadata()).execute().await.map_err(error)?;
        }
        Ok(())
    }
    .await;
    if result.is_err()
        && let Some(upload) = upload
        && let Err(abort_error) = upload.abort().await
    {
        tracing::warn!(%abort_error, "Could not abort failed upload");
    }
    result
}

fn metadata() -> worker::HttpMetadata {
    worker::HttpMetadata {
        content_type: Some("application/octet-stream".into()),
        ..Default::default()
    }
}

async fn upload_part(upload: &worker::MultipartUpload, parts: &mut Vec<worker::UploadedPart>, bytes: Vec<u8>) -> Result<(), BucketError> {
    let number = parts.len() as u16 + 1;
    parts.push(upload.upload_part(number, bytes).await.map_err(error)?);
    Ok(())
}
