use super::bucket::{ProjectBucket, ResultKey};
use super::user::{UserId, Username};
use serde::{Deserialize, Serialize};
use std::fmt;
use time::{OffsetDateTime, UtcOffset};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ty")]
pub enum OperationLog {
    #[serde(rename = "mup")]
    ManuscriptUpdated {
        #[serde(rename = "uid")]
        user_id: UserId,
        #[serde(rename = "un")]
        username: Username,
        #[serde(rename = "fn")]
        filename: String,
    },
    #[serde(rename = "env")]
    EnvironmentUpdated,
    #[serde(rename = "bs")]
    BuildStarted {
        #[serde(rename = "rid")]
        run_id: ResultKey,
    },
    #[serde(rename = "bsc")]
    BuildSucceeded {
        #[serde(rename = "rid")]
        run_id: ResultKey,
    },
    #[serde(rename = "bfa")]
    BuildFailed {
        #[serde(rename = "rid")]
        run_id: ResultKey,
        #[serde(rename = "ec")]
        exit_code: Option<i32>,
    },
}

impl fmt::Display for OperationLog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OperationLog::ManuscriptUpdated { username, filename, .. } => write!(f, "{username}さんが原稿を更新しました({filename})"),
            OperationLog::EnvironmentUpdated => f.write_str("ビルド環境が更新されました"),
            OperationLog::BuildStarted { .. } => f.write_str("ビルドが開始されました"),
            OperationLog::BuildSucceeded { .. } => f.write_str("ビルドが正常に完了しました"),
            OperationLog::BuildFailed { .. } => f.write_str("ビルドが失敗しました"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct OperationLogEntry {
    pub timestamp: OffsetDateTime,
    pub event: OperationLog,
}

#[derive(Debug)]
pub struct OperationLogPage {
    pub entries: Vec<OperationLogEntry>,
    pub next_cursor: Option<String>,
}

impl OperationLogEntry {
    pub fn datetime(&self) -> String {
        let datetime = self.timestamp.to_offset(UtcOffset::from_hms(9, 0, 0).unwrap());
        let (year, month, day) = datetime.to_calendar_date();
        let (hour, minute, second) = datetime.to_hms();
        format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02} JST")
    }
}

/// 書き込みは失敗しない
pub async fn record(project: &impl ProjectBucket, event: OperationLog) {
    if let Err(error) = project.append_log(&event).await {
        tracing::error!(%error, ?event, "Could not save operation log");
    }
}
