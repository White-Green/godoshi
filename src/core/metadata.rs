use super::bucket::{BucketError, Data};
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_FIELDS: usize = 64;
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_FILE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum MetadataValidationError {
    #[error("項目は64個以下にしてください。")]
    TooManyFields,
    #[error("項目キーは重複しない英小文字・数字・アンダースコアで、先頭を英小文字にしてください（64文字以内）。")]
    InvalidSchemaKey,
    #[error("項目名または説明の長さが不正です。")]
    InvalidLabelOrDescription,
    #[error("max_length は1〜65536で指定してください。")]
    InvalidMaxLength,
    #[error("accept は1024バイト以内にしてください。")]
    InvalidAccept,
    #[error("ファイルは5 MiB以下にしてください。")]
    FileTooLarge,
    #[error("ファイル名が不正です。")]
    InvalidFilename,
    #[error("項目キーまたは項目数が不正です。")]
    InvalidUpdateFields,
    #[error("文面の合計は64 KiB以下にしてください。")]
    TextTooLarge,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataSchema {
    pub fields: Vec<FieldDefinition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDefinition {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub description: String,
    #[serde(flatten)]
    pub kind: FieldKind,
}

/// Persisted project configuration shared through ProjectStore.
/// Only the app interprets these hints to render forms; storage never applies them to values.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum FieldKind {
    String {
        #[serde(default)]
        multiline: bool,
        #[serde(default)]
        max_length: Option<usize>,
    },
    File {
        #[serde(default)]
        accept: Option<String>,
    },
}

pub fn valid_field_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key.as_bytes()[0].is_ascii_lowercase()
        && key.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

impl MetadataSchema {
    pub fn validate(&self) -> Result<(), MetadataValidationError> {
        if self.fields.len() > MAX_FIELDS {
            return Err(MetadataValidationError::TooManyFields);
        }
        let mut keys = BTreeSet::new();
        for field in &self.fields {
            if !valid_field_key(&field.key) || !keys.insert(&field.key) {
                return Err(MetadataValidationError::InvalidSchemaKey);
            }
            if field.label.trim().is_empty() || field.label.len() > 1024 || field.description.len() > 4096 {
                return Err(MetadataValidationError::InvalidLabelOrDescription);
            }
            if let FieldKind::String { max_length, .. } = &field.kind
                && max_length.is_some_and(|length| length == 0 || length > MAX_TEXT_BYTES)
            {
                return Err(MetadataValidationError::InvalidMaxLength);
            }
            if let FieldKind::File { accept } = &field.kind
                && accept.as_ref().is_some_and(|accept| accept.len() > 1024)
            {
                return Err(MetadataValidationError::InvalidAccept);
            }
        }
        Ok(())
    }
}

pub struct MetadataFile {
    bytes: Bytes,
    filename: String,
}

impl MetadataFile {
    pub fn new(bytes: Bytes, filename: String) -> Result<Self, MetadataValidationError> {
        if bytes.len() > MAX_FILE_BYTES {
            return Err(MetadataValidationError::FileTooLarge);
        }
        // Reject client paths before using the filename's extension in a public name.
        if filename.is_empty()
            || filename.len() > 255
            || filename == "."
            || filename == ".."
            || filename.chars().any(|c| c.is_control() || matches!(c, '/' | '\\'))
        {
            return Err(MetadataValidationError::InvalidFilename);
        }
        Ok(Self { bytes, filename })
    }
    pub fn filename(&self) -> &str {
        &self.filename
    }
    pub fn bytes(&self) -> &Bytes {
        &self.bytes
    }
}

pub enum MetadataChange {
    String(String),
    File(MetadataFile),
    Remove,
}

#[derive(Default)]
pub struct MetadataUpdate(pub BTreeMap<String, MetadataChange>);

impl MetadataUpdate {
    pub fn validate(&self) -> Result<(), MetadataValidationError> {
        if self.0.len() > MAX_FIELDS || self.0.keys().any(|key| !valid_field_key(key)) {
            return Err(MetadataValidationError::InvalidUpdateFields);
        }
        let text_bytes: usize = self
            .0
            .values()
            .map(|v| match v {
                MetadataChange::String(s) => s.len(),
                _ => 0,
            })
            .sum();
        if text_bytes > MAX_TEXT_BYTES {
            return Err(MetadataValidationError::TextTooLarge);
        }
        Ok(())
    }
}

/// Public values only. Internal R2 keys never leave the storage adapter.
#[derive(Debug, Clone, Default)]
pub struct AuthorMetadata {
    pub values: BTreeMap<String, String>,
    pub files: BTreeMap<String, String>,
}

pub fn asset_filename(key: &str, original_filename: &str) -> String {
    match original_filename.split_once('.') {
        Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => format!("{key}.{extension}"),
        _ => key.to_owned(),
    }
}

pub fn asset_path(filename: &str) -> String {
    format!("assets/{filename}")
}

pub trait AuthorMetadataBucket: Send + Sync {
    fn load(&self) -> impl Future<Output = Result<AuthorMetadata, BucketError>> + Send;
    fn save(&self, update: MetadataUpdate) -> impl Future<Output = Result<(), BucketError>> + Send;
    fn file(&self, key: &str) -> impl Future<Output = Result<Option<(String, Data)>, BucketError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_filenames_use_field_keys_and_only_keep_extensions() {
        assert_eq!(asset_filename("icon", "first.png"), "icon.png");
        assert_eq!(asset_filename("source", "original.tar.gz"), "source.tar.gz");
        for name in ["README", ".env", "trailing."] {
            assert_eq!(asset_filename("attachment", name), "attachment");
        }
    }

    #[test]
    fn schemas_reject_duplicate_and_invalid_field_keys() {
        let json = r#"{"fields":[{"key":"pen_name","label":"筆名","type":"string"},
            {"key":"portrait","label":"画像","type":"file","accept":"image/*"}]}"#;
        let mut schema: MetadataSchema = serde_json::from_str(json).unwrap();
        schema.validate().unwrap();
        schema.fields.push(schema.fields[0].clone());
        assert!(schema.validate().is_err());
        for key in ["", "../icon", "a/b", "A", "a.b", "1a"] {
            assert!(!valid_field_key(key));
        }
    }

    #[test]
    fn files_reject_invalid_names_and_oversized_contents() {
        for name in ["", ".", "..", "../icon", "a/b", "a\\b", "a\n"] {
            assert!(MetadataFile::new(Bytes::new(), name.into()).is_err());
        }
        assert!(MetadataFile::new(Bytes::from(vec![0; MAX_FILE_BYTES + 1]), "large.bin".into()).is_err());
    }
}
