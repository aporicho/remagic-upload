use crate::catalog::ObjectRecord;
use crate::sync_scope::SyncSelection;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// Wire schema 3 adds an initiator-owned sync item selection before snapshots.
// Keep this independent from the on-disk catalog schema: peers may evolve the
// transport without forcing a local database migration.
pub const PROTOCOL_SCHEMA: u32 = 3;
pub const CLOCK_SKEW_LIMIT_MS: i64 = 2 * 60 * 1000;
pub const CHUNK_BYTES: usize = 32 * 1024;
pub const MAX_READING_BYTES: usize = 16 * 1024 * 1024;

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
        reading_bytes: u64,
    },
    Record(ObjectRecord),
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

#[derive(Debug)]
pub struct Snapshot {
    pub records: Vec<ObjectRecord>,
    pub reading: Vec<u8>,
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
}
