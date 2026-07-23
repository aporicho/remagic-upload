use crate::catalog::ObjectRecord;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PROTOCOL_SCHEMA: u32 = crate::catalog::SYNC_SCHEMA;
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
    SnapshotStart {
        records: u32,
        reading_bytes: u64,
    },
    Record(ObjectRecord),
    Data(Vec<u8>),
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
}
