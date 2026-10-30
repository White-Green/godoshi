use super::{R2Project, error};
use crate::core::bucket::{BucketError, ResultKey, generate_key};
use crate::core::operation_log::{OperationLog, OperationLogEntry, OperationLogPage};
use futures::{StreamExt, TryStreamExt};

pub(super) async fn append(project: &R2Project<'_>, event: &OperationLog) -> Result<(), BucketError> {
    let key = generate_key(time::OffsetDateTime::now_utc())?;
    let data = serde_json::to_vec(event).map_err(std::io::Error::other)?;
    project
        .bucket
        .put(format!("{}logs/{}.json", project.prefix, key.as_str()), data)
        .http_metadata(worker::HttpMetadata {
            content_type: Some("application/json".into()),
            ..Default::default()
        })
        .execute()
        .await
        .map_err(error)?;
    Ok(())
}

pub(super) async fn list(project: &R2Project<'_>, limit: u32, cursor: Option<&str>) -> Result<OperationLogPage, BucketError> {
    let prefix = format!("{}logs/", project.prefix);
    // Fetch only this page, including its bodies. Never follow the cursor here.
    let mut list = project.bucket.list().prefix(&prefix).limit(limit.clamp(1, 100));
    if let Some(cursor) = cursor {
        list = list.cursor(cursor.to_owned());
    }
    let objects = list.execute().await.map_err(error)?;
    let next_cursor = if objects.truncated() {
        Some(objects.cursor().expect("Truncated R2 listing must include a cursor"))
    } else {
        None
    };
    let entries = objects.objects().into_iter().filter_map(|object| {
        let name = object.key();
        let key = name
            .strip_prefix(&prefix)
            .and_then(|name| name.strip_suffix(".json"))
            .and_then(|key| ResultKey::new(key.to_owned()))?;
        Some(async move {
            let Some(object) = project.bucket.get(name).execute().await.map_err(error)? else {
                return Ok(None);
            };
            let data = object
                .body()
                .expect("Unconditional R2 GET must have a body")
                .bytes()
                .await
                .map_err(error)?;
            let event = serde_json::from_slice(&data).map_err(std::io::Error::other)?;
            Ok::<_, BucketError>(Some(OperationLogEntry {
                timestamp: key.built_datetime(),
                event,
            }))
        })
    });
    let mut entries = futures::stream::iter(entries)
        .buffered(8)
        .try_filter_map(futures::future::ok)
        .try_collect::<Vec<_>>()
        .await?;
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.timestamp));
    Ok(OperationLogPage { entries, next_cursor })
}
