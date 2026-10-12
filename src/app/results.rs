use super::storage_error;
use crate::auth::AuthState;
use axum::http::{StatusCode, header};
use axum::response::Response;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::Deserialize;
use std::collections::BTreeSet;
use time::{OffsetDateTime, UtcOffset};
use worker::Bucket;
use crate::project::ProjectId;

#[derive(Debug)]
pub(super) struct ResultRun {
    pub name: String,
    pub key: String,
}

impl ResultRun {
    pub fn new(run_id: &str) -> Self {
        Self {
            name: run_datetime(run_id).unwrap_or_else(|| run_id.to_owned()),
            key: encode(run_id),
        }
    }
}

#[derive(Debug)]
pub(super) struct ResultFile {
    pub name: String,
    pub key: String,
    pub size: u64,
}

#[derive(Deserialize)]
pub(super) struct RunPath {
    pub run_id: String,
}

#[derive(Deserialize)]
pub(super) struct FileQuery {
    pub key: String,
    #[serde(default)]
    pub download: bool,
}

pub(super) async fn list_runs(bucket: &Bucket, project_id: ProjectId, username: Option<&str>) -> Result<Vec<ResultRun>, (StatusCode, &'static str)> {
    let prefix = format!("projects/{}/results/", project_id);
    let author_prefix = username.map(|username| format!("authors/{username}/"));
    let mut cursor = None;
    let mut runs = BTreeSet::new();
    loop {
        let mut list = bucket.list().prefix(&prefix).limit(100);
        if author_prefix.is_none() {
            list = list.delimiter("/");
        }
        if let Some(cursor) = cursor.take() {
            list = list.cursor(cursor);
        }
        let objects = list.execute().await.map_err(storage_error)?;
        if let Some(author_prefix) = &author_prefix {
            // The author sits below the run ID, so filter every metadata page by
            // the exact author directory and expose only runs with their files.
            for object in objects.objects() {
                let key = object.key();
                let Some((run, path)) = key.strip_prefix(&prefix).and_then(|key| key.split_once('/')) else {
                    continue;
                };
                if let Some(filename) = path.strip_prefix(author_prefix)
                    && valid_key(filename)
                    && valid_key(run)
                {
                    runs.insert(run.to_owned());
                }
            }
        } else {
            runs.extend(objects.delimited_prefixes().into_iter().map(|key| {
                key.strip_prefix(&prefix)
                    .expect("Listed prefix must be in the results prefix")
                    .trim_end_matches('/')
                    .to_owned()
            }));
        }
        if !objects.truncated() {
            break;
        }
        cursor = Some(objects.cursor().expect("Truncated R2 listing must include a cursor"));
    }
    // Reverse timestamps put newer runs first in ascending key order.
    Ok(runs.into_iter().map(|run| ResultRun::new(&run)).collect())
}

pub(super) async fn list_files(bucket: &Bucket, prefix: &str) -> Result<Vec<ResultFile>, (StatusCode, &'static str)> {
    let mut cursor = None;
    let mut files = Vec::new();
    loop {
        let mut list = bucket.list().prefix(prefix).limit(100);
        if let Some(cursor) = cursor.take() {
            list = list.cursor(cursor);
        }
        let objects = list.execute().await.map_err(storage_error)?;
        files.extend(objects.objects().into_iter().filter_map(|object| {
            let key = object.key();
            let name = key.strip_prefix(prefix).expect("Listed object must be in the requested prefix");
            if !valid_key(name) {
                return None;
            }
            Some(ResultFile {
                key: encode(name),
                name: name.to_owned(),
                size: object.size(),
            })
        }));
        if !objects.truncated() {
            break;
        }
        cursor = Some(objects.cursor().expect("Truncated R2 listing must include a cursor"));
    }
    files.sort_by(|left, right| right.name.cmp(&left.name));
    Ok(files)
}

pub(super) async fn serve_file(bucket: &Bucket, prefix: &str, query: FileQuery) -> Result<Response, (StatusCode, &'static str)> {
    let key = format!("{prefix}{}", query.key);
    if key.len() > 1024 || !valid_key(&query.key) {
        return Err((StatusCode::BAD_REQUEST, "ファイル名が不正です。"));
    }
    let object = bucket
        .get(key)
        .execute()
        .await
        .map_err(storage_error)?
        .ok_or((StatusCode::NOT_FOUND, "ファイルが見つかりません。"))?;
    let body = object
        .body()
        .expect("Unconditional R2 GET must have a body")
        .response_body()
        .map_err(storage_error)?;
    let mut response: Response = worker::Response::from_body(body).map_err(storage_error)?.into();
    let filename = query.key.rsplit('/').next().unwrap();
    let disposition = if query.download { "attachment" } else { "inline" };
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        format!("{disposition}; filename=\"download\"; filename*=UTF-8''{}", encode(filename))
            .parse()
            .unwrap(),
    );
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, object.size().to_string().parse().unwrap());
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, preview_type(filename).parse().unwrap());
    response.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS, "nosniff".parse().unwrap());
    response
        .headers_mut()
        .insert(header::CONTENT_SECURITY_POLICY, "sandbox; default-src 'none'".parse().unwrap());
    Ok(response)
}

fn run_datetime(run_id: &str) -> Option<String> {
    let (reverse_timestamp, _) = run_id.split_once('-')?;
    let timestamp = 1_000_000_000_000_i64.checked_sub(reverse_timestamp.parse().ok()?)?;
    let datetime = OffsetDateTime::from_unix_timestamp(timestamp)
        .ok()?
        .checked_to_offset(UtcOffset::from_hms(9, 0, 0).unwrap())?;
    let (year, month, day) = datetime.to_calendar_date();
    let (hour, minute, second) = datetime.to_hms();
    Some(format!("{year:04}-{:02}-{day:02} {hour:02}:{minute:02}:{second:02} JST", month as u8))
}

pub(super) fn run_prefix(auth_state: &AuthState, run_id: &str) -> Result<String, (StatusCode, &'static str)> {
    let prefix = format!("projects/{}/results/{run_id}/", auth_state.project.id);
    if !valid_key(run_id) || run_id.contains('/') || prefix.len() > 1024 {
        return Err((StatusCode::BAD_REQUEST, "実行IDが不正です。"));
    }
    Ok(prefix)
}

fn encode(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

pub(super) fn valid_key(key: &str) -> bool {
    !key.is_empty() && !key.chars().any(char::is_control) && !key.contains('\\') && key.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

fn preview_type(filename: &str) -> &'static str {
    match filename.rsplit('.').next().unwrap_or_default().to_ascii_lowercase().as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "txt" | "log" | "csv" | "json" | "md" | "html" | "svg" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
