use crate::catalog::ObjectRecord;
use crate::sync_scope::SyncSelection;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

// Wire schema 6 renames KOReader font-size sync to document-settings sync.
// Keep this independent from the on-disk catalog schema: peers may evolve the
// transport without forcing a local database migration.
pub const PROTOCOL_SCHEMA: u32 = 6;
pub const CLOCK_SKEW_LIMIT_MS: i64 = 2 * 60 * 1000;
pub const CHUNK_BYTES: usize = 32 * 1024;
pub const MAX_READING_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_SNAPSHOT_RECORDS: u32 = 100_000;
pub const MAX_LABEL_BYTES: usize = 512;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Wire {
    Hello {
        schema: u32,
        id: String,
        name: String,
        time_ms: i64,
    },
    Scope(SyncSelection),
    SnapshotStart {
        records: u32,
        labels: u32,
        reading_bytes: u64,
    },
    Record(ObjectRecord),
    RecordLabel(RecordLabel),
    Data(Vec<u8>),
    DataAck(u64),
    SnapshotEnd,
    Get {
        kind: String,
        path: String,
    },
    PutStart(ObjectRecord),
    Resume(u64),
    FileEnd,
    Applied,
    ReadingStart(u64),
    Done,
    Error(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecordLabel {
    pub kind: String,
    pub path: String,
    pub label: String,
}

pub type RecordKey = (String, String);

#[derive(Debug)]
pub struct Snapshot {
    pub records: Vec<ObjectRecord>,
    pub labels: BTreeMap<RecordKey, String>,
    pub reading: Vec<u8>,
}

impl Snapshot {
    pub fn label_for(&self, record: &ObjectRecord) -> String {
        self.labels
            .get(&record_key(record))
            .cloned()
            .unwrap_or_else(|| record.path.clone())
    }
}

pub fn record_key(record: &ObjectRecord) -> RecordKey {
    (record.kind.clone(), record.path.clone())
}

pub fn expect_hello(value: Wire) -> Result<(String, String, i64), ProtocolError> {
    match value {
        Wire::Hello {
            schema,
            id,
            name,
            time_ms,
        } if schema == PROTOCOL_SCHEMA && id.len() == 16 && !name.trim().is_empty() => {
            Ok((id, name, time_ms))
        }
        Wire::Hello { schema, .. } => Err(ProtocolError::Schema(schema)),
        _ => Err(ProtocolError::Unexpected),
    }
}

pub fn validate_clock(local_ms: i64, remote_ms: i64) -> Result<(), ProtocolError> {
    if local_ms.abs_diff(remote_ms) > CLOCK_SKEW_LIMIT_MS as u64 {
        Err(ProtocolError::ClockSkew {
            local_ms,
            remote_ms,
        })
    } else {
        Ok(())
    }
}

pub fn expect_data_ack(value: Wire, expected: u64) -> Result<(), ProtocolError> {
    match value {
        Wire::DataAck(received) if received == expected => Ok(()),
        _ => Err(ProtocolError::Unexpected),
    }
}

pub fn clean_label(label: &str) -> Option<String> {
    if label.len() > MAX_LABEL_BYTES || label.chars().any(char::is_control) {
        return None;
    }
    let trimmed = label.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("peer uses unsupported sync schema {0}")]
    Schema(u32),
    #[error("peer sent an unexpected sync message")]
    Unexpected,
    #[error("device clocks differ too much ({local_ms} vs {remote_ms})")]
    ClockSkew { local_ms: i64, remote_ms: i64 },
    #[error("peer snapshot exceeds the supported size")]
    SnapshotTooLarge,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_dangerous_clock_skew() {
        assert!(validate_clock(1_000_000, 1_000_001).is_ok());
        assert!(validate_clock(0, CLOCK_SKEW_LIMIT_MS + 1).is_err());
    }

    #[test]
    fn data_ack_must_match_the_exact_committed_offset() {
        assert!(expect_data_ack(Wire::DataAck(32_768), 32_768).is_ok());
        assert!(expect_data_ack(Wire::DataAck(32_767), 32_768).is_err());
    }

    #[test]
    fn labels_must_be_short_printable_text() {
        assert_eq!(clean_label("  论语.epub  ").as_deref(), Some("论语.epub"));
        assert!(clean_label("").is_none());
        assert!(clean_label("bad\nlabel").is_none());
        assert!(clean_label(&"a".repeat(MAX_LABEL_BYTES + 1)).is_none());
    }
}
