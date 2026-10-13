use crate::core::project::ProjectId;
use crate::core::user::Username;
use bytes::Bytes;
use futures::Stream;
use futures::stream::BoxStream;
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use std::fmt::Display;
use std::str::FromStr;
use time::{OffsetDateTime, UtcOffset};

pub const S3_RESOURCES_BUCKET: &str = "resources";
pub const S3_RESULTS_BUCKET: &str = "results";
pub const MAX_RESOURCE_FILES: usize = 100;

#[derive(Debug, thiserror::Error)]
pub enum BucketError {
    #[error("Invalid storage key")]
    InvalidKey,
    #[error("At most {MAX_RESOURCE_FILES} resource files are allowed")]
    TooManyFiles,
    #[error("The file does not belong to this resource list")]
    InvalidFile,
    #[error("The resource list has changed; reload it before updating")]
    Conflict,
    #[error("Could not read upload data")]
    Read(#[source] std::io::Error),
    #[error(transparent)]
    Backend(#[from] std::io::Error),
}

pub struct Data {
    pub size: u64,
    pub stream: BoxStream<'static, Result<Bytes, std::io::Error>>,
}

impl From<Vec<u8>> for Data {
    fn from(bytes: Vec<u8>) -> Self {
        Self {
            size: bytes.len() as u64,
            stream: Box::pin(futures::stream::iter([Ok(Bytes::from(bytes))])),
        }
    }
}

pub fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 1024
        && !key.chars().any(char::is_control)
        && !key.contains('\\')
        && key.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct ResultKey(String);

impl ResultKey {
    pub fn new(key: String) -> Option<Self> {
        let (reversed_timestamp, nonce) = key.split_once('-')?;
        if !(0..1_000_000_000_000_i64).contains(&i64::from_str(reversed_timestamp).ok()?) {
            return None;
        }
        if !nonce.chars().all(|c| c.is_ascii_digit()) || !(1..=20).contains(&nonce.len()) {
            return None;
        }
        Some(ResultKey(key))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn for_build(now: OffsetDateTime, nonce: u64) -> Self {
        Self(format!("{:012}-{nonce}", 1_000_000_000_000_i64 - now.unix_timestamp()))
    }

    pub fn built_datetime(&self) -> OffsetDateTime {
        let (reversed_timestamp, _) = self.0.split_once('-').unwrap();
        OffsetDateTime::from_unix_timestamp(1_000_000_000_000_i64 - i64::from_str(reversed_timestamp).unwrap()).unwrap()
    }
}

impl AsRef<str> for ResultKey {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl<'de> Deserialize<'de> for ResultKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).ok_or_else(|| serde::de::Error::custom("invalid result key"))
    }
}

#[derive(Debug, Clone)]
pub struct BuildResult {
    pub built_datetime: OffsetDateTime,
    pub key: ResultKey,
}

impl Display for BuildResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let datetime = self.built_datetime.to_offset(UtcOffset::from_hms(9, 0, 0).unwrap());
        let (year, month, day) = datetime.to_calendar_date();
        let (hour, minute, second) = datetime.to_hms();
        write!(f, "{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} JST")
    }
}

impl From<ResultKey> for BuildResult {
    fn from(key: ResultKey) -> Self {
        Self {
            built_datetime: key.built_datetime(),
            key,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    pub key: String,
    pub size: u64,
}

pub trait Bucket: Send + Sync {
    type ProjectBucket<'a>: ProjectBucket + 'a
    where
        Self: 'a;
    fn project(&self, project_id: ProjectId) -> Self::ProjectBucket<'_>;
}

pub trait ProjectBucket: Send + Sync {
    type ResourceBucket<'a>: ResourceBucket + 'a
    where
        Self: 'a;
    type UserResourceBucket<'a>: UserResourceBucket + 'a
    where
        Self: 'a;
    type ResultListBucket<'a>: ResultListBucket + 'a
    where
        Self: 'a;
    type UserResultBucket<'a>: UserResultListBucket + 'a
    where
        Self: 'a;
    fn resource(&self) -> Self::ResourceBucket<'_>;
    fn resource_by_user(&self, username: Username) -> Self::UserResourceBucket<'_>;
    fn result(&self) -> Self::ResultListBucket<'_>;
    fn result_by_user(&self, username: Username) -> Self::UserResultBucket<'_>;
}

/// ビルドの入力として使うresource
pub trait ResourceBucket: Send + Sync {
    fn as_s3(&self) -> impl s3s::S3;
    fn upload_environment(
        &self,
        data: impl Stream<Item = Result<Bytes, std::io::Error>> + Send,
    ) -> impl Future<Output = Result<(), BucketError>> + Send;
    fn download(&self, key: &str) -> impl Future<Output = Result<Option<Data>, BucketError>> + Send;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Extension<'a>(&'a str);

impl<'a> Extension<'a> {
    pub fn new(extension: &'a str) -> Option<Self> {
        (!extension.is_empty() && !extension.contains(['/', '\\']) && !extension.chars().any(|c| c.is_control() || c.is_whitespace()))
            .then_some(Self(extension))
    }
}

impl AsRef<str> for Extension<'_> {
    fn as_ref(&self) -> &str {
        self.0
    }
}

/// opaqueなデータとしてのFile
pub trait FileObject<'a>: Send {
    fn from_data(data: BoxStream<'a, Result<Bytes, std::io::Error>>, extension: Option<Extension<'_>>) -> Self;
    fn extension(&self) -> Option<Extension<'_>>;
}

/// ユーザ1人に対応するresource群
pub trait UserResourceBucket: Send + Sync {
    type FileObject<'a>: FileObject<'a>;
    fn current_file_list<'a>(&self) -> impl Future<Output = Result<Vec<Self::FileObject<'a>>, BucketError>> + Send;
    fn update(&self, files: Vec<Self::FileObject<'_>>) -> impl Future<Output = Result<(), BucketError>> + Send;
}

pub fn resource_file_key(index: usize, extension: Option<Extension<'_>>) -> String {
    match extension {
        Some(extension) => format!("{}.{}", index + 1, extension.as_ref()),
        None => (index + 1).to_string(),
    }
}

/// ビルド結果すべて
pub trait ResultListBucket: Send + Sync {
    type ResultBucket<'a>: ResultBucket + 'a
    where
        Self: 'a;
    fn generate_key(&self, now: OffsetDateTime) -> Result<ResultKey, BucketError>;
    fn list(&self) -> impl Future<Output = Result<Vec<BuildResult>, BucketError>> + Send;
    fn result(&self, key: &ResultKey) -> Self::ResultBucket<'_>;
}

/// 1回分のビルド結果
pub trait ResultBucket: Send + Sync {
    fn as_s3(&self) -> impl s3s::S3;
    fn list(&self) -> impl Future<Output = Result<Vec<File>, BucketError>> + Send;
    fn upload(
        &self,
        key: &str,
        data: impl Stream<Item = Result<Bytes, std::io::Error>> + Send,
    ) -> impl Future<Output = Result<(), BucketError>> + Send;
    fn download(&self, key: &str) -> impl Future<Output = Result<Option<Data>, BucketError>> + Send;
}

/// ビルド結果のうち、ユーザが参照できる分
pub trait UserResultListBucket: Send + Sync {
    type ResultBucket<'a>: UserResultBucket + 'a
    where
        Self: 'a;
    /// このユーザが参照できるファイルを1つ以上含むビルドだけを返す。
    fn list(&self) -> impl Future<Output = Result<Vec<BuildResult>, BucketError>> + Send;
    fn result(&self, key: &ResultKey) -> Self::ResultBucket<'_>;
}

/// ユーザが参照できるビルド結果1回分
pub trait UserResultBucket: ResultBucket {}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn test_extension_new() {
        assert_eq!(Extension::new("pdf").unwrap().as_ref(), "pdf");
        assert_eq!(Extension::new("tar.gz").unwrap().as_ref(), "tar.gz");
        assert_eq!(Extension::new("PDF").unwrap().as_ref(), "PDF");
        assert_eq!(Extension::new("日本語").unwrap().as_ref(), "日本語");
        assert!(Extension::new("").is_none());
        assert!(Extension::new(" ").is_none());
        assert!(Extension::new("\u{3000}").is_none());
        assert!(Extension::new("../pdf").is_none());
        assert!(Extension::new("pdf/other").is_none());
        assert!(Extension::new("p\\df").is_none());
        assert!(Extension::new("p\0df").is_none());
        assert!(Extension::new("p\ndf").is_none());
        assert!(Extension::new("p\u{007f}df").is_none());
    }

    #[test]
    fn test_result_key_new() {
        assert!(ResultKey::new("".to_owned()).is_none());
        assert!(ResultKey::new("1-0".to_owned()).is_some());
        assert!(ResultKey::new("1-".to_owned()).is_none());
        assert!(ResultKey::new("1-x".to_owned()).is_none());
        assert!(ResultKey::new("1--0".to_owned()).is_none());
        assert!(ResultKey::new("1-184467440737095516150".to_owned()).is_none());
        assert!(ResultKey::new("000000000001-0".to_owned()).is_some());
        assert!(ResultKey::new("000000000000-0".to_owned()).is_some());
        assert!(ResultKey::new("999999999999-0".to_owned()).is_some());
        assert!(ResultKey::new("1000000000000-0".to_owned()).is_none());
    }

    #[test]
    fn test_result_key_for_build() {
        assert_eq!(
            ResultKey::for_build(datetime!(1970-01-01 09:00:00 +00:00), 123456).0,
            "999999967600-123456"
        );
        assert_eq!(
            ResultKey::for_build(datetime!(2001-02-03 04:05:06 +09:00), 123456).0,
            "999018859294-123456"
        );
        for nonce in [0, 9_999_999_999_999_999_999, 10_000_000_000_000_000_000, u64::MAX] {
            let key = ResultKey::for_build(datetime!(2026-10-18 00:00:00 +00:00), nonce);
            assert_eq!(ResultKey::new(key.as_str().to_owned()), Some(key), "nonce: {nonce}");
        }
    }

    #[test]
    fn test_result_key_datetime() {
        assert_eq!(
            ResultKey::new("999999999999-0".to_owned()).unwrap().built_datetime(),
            OffsetDateTime::from_unix_timestamp(1).unwrap()
        );
        assert_eq!(
            ResultKey::new("995705032704-0".to_owned()).unwrap().built_datetime(),
            OffsetDateTime::from_unix_timestamp(u32::MAX as i64 + 1).unwrap()
        );
    }
}
