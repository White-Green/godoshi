use super::checksum::{Algorithm, Kind, Multipart, Part, Sum};
use s3s::{S3Error, S3ErrorCode, S3Result};
use serde::Deserialize;
use std::collections::HashMap;
use worker::D1Database;

pub(super) fn db_error(error: worker::Error) -> S3Error {
    tracing::error!(%error, "S3 multipart database operation failed");
    S3Error::new(S3ErrorCode::InternalError)
}

pub(super) async fn create(db: &D1Database, key: &str, upload_id: &str, checksum: &Multipart) -> S3Result<()> {
    worker::query!(
        db,
        "INSERT INTO s3_multipart_upload (object_key, r2_upload_id, algorithm) VALUES (?, ?, ?)",
        key,
        upload_id,
        checksum.algorithm,
    )
    .expect("Multipart upload insert parameters must be valid D1 values")
    .run()
    .await
    .map_err(db_error)?;
    Ok(())
}

pub(super) async fn get(db: &D1Database, key: &str, upload_id: &str) -> S3Result<Multipart> {
    worker::query!(
        db,
        "SELECT algorithm FROM s3_multipart_upload WHERE object_key = ? AND r2_upload_id = ?",
        key,
        upload_id,
    )
    .expect("Multipart upload query parameters must be valid D1 values")
    .first::<Multipart>(None)
    .await
    .map_err(db_error)?
    .ok_or_else(|| S3Error::new(S3ErrorCode::NoSuchUpload))
}

pub(super) async fn save_part(db: &D1Database, key: &str, upload_id: &str, number: u16, etag: &str, part: &Part) -> S3Result<()> {
    worker::query!(
        db,
        "INSERT INTO s3_multipart_part (object_key, r2_upload_id, part_number, etag, length, algorithm, checksum)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (object_key, r2_upload_id, part_number, etag) DO UPDATE SET
             length = excluded.length, algorithm = excluded.algorithm, checksum = excluded.checksum",
        key,
        upload_id,
        number,
        etag,
        part.length,
        part.sum.algorithm,
        part.sum.value,
    )
    .expect("Multipart part insert parameters must be valid D1 values")
    .run()
    .await
    .map_err(db_error)?;
    Ok(())
}

pub(super) async fn parts(db: &D1Database, key: &str, upload_id: &str) -> S3Result<HashMap<(u16, String), Part>> {
    #[derive(Deserialize)]
    struct Row {
        part_number: u16,
        etag: String,
        length: u64,
        algorithm: Algorithm,
        checksum: String,
    }
    let rows = worker::query!(
        db,
        "SELECT part_number, etag, length, algorithm, checksum FROM s3_multipart_part WHERE object_key = ? AND r2_upload_id = ?",
        key,
        upload_id,
    )
    .expect("Multipart parts query parameters must be valid D1 values")
    .all()
    .await
    .map_err(db_error)?
    .results::<Row>()
    .expect("Multipart part query results must match the schema");
    Ok(rows
        .into_iter()
        .map(|row| {
            (
                (row.part_number, row.etag),
                Part {
                    length: row.length,
                    sum: Sum {
                        algorithm: row.algorithm,
                        value: row.checksum,
                        kind: Kind::FullObject,
                    },
                },
            )
        })
        .collect())
}

pub(super) async fn remove(db: &D1Database, key: &str, upload_id: &str) {
    let result = worker::query!(
        db,
        "DELETE FROM s3_multipart_upload WHERE object_key = ? AND r2_upload_id = ?",
        key,
        upload_id,
    )
    .expect("Multipart upload delete parameters must be valid D1 values")
    .run()
    .await;
    if let Err(error) = result {
        tracing::warn!(%error, "Could not remove completed/aborted S3 multipart metadata");
    }
}
