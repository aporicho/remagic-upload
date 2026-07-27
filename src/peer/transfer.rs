use crate::catalog::ObjectRecord;
use crate::server::SharedStatus;

use super::error::PeerError;
use super::noise::NoiseChannel;
use super::protocol::{
    self, clean_label, expect_data_ack, record_key, ProtocolError, RecordLabel, Snapshot, Wire,
    CHUNK_BYTES,
};
use super::storage::PeerStorage;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};

pub(super) fn send_snapshot(
    channel: &mut NoiseChannel,
    snapshot: &Snapshot,
) -> Result<(), PeerError> {
    channel.send(&Wire::SnapshotStart {
        records: snapshot
            .records
            .len()
            .try_into()
            .map_err(|_| ProtocolError::SnapshotTooLarge)?,
        labels: snapshot
            .labels
            .len()
            .try_into()
            .map_err(|_| ProtocolError::SnapshotTooLarge)?,
        reading_bytes: snapshot.reading.len() as u64,
    })?;
    for record in &snapshot.records {
        channel.send(&Wire::Record(record.clone()))?;
    }
    for ((kind, path), label) in &snapshot.labels {
        channel.send(&Wire::RecordLabel(RecordLabel {
            kind: kind.clone(),
            path: path.clone(),
            label: label.clone(),
        }))?;
    }
    send_blob(channel, &snapshot.reading)?;
    channel.send(&Wire::SnapshotEnd)?;
    Ok(())
}

pub(super) fn receive_snapshot(channel: &mut NoiseChannel) -> Result<Snapshot, PeerError> {
    let (count, label_count, reading_bytes) = match channel.receive()? {
        Wire::SnapshotStart {
            records,
            labels,
            reading_bytes,
        } if records <= protocol::MAX_SNAPSHOT_RECORDS
            && labels <= records
            && reading_bytes <= protocol::MAX_READING_BYTES as u64 =>
        {
            (records, labels, reading_bytes)
        }
        _ => return Err(ProtocolError::SnapshotTooLarge.into()),
    };
    let mut records = Vec::with_capacity(count as usize);
    let mut keys = BTreeSet::new();
    for _ in 0..count {
        match channel.receive()? {
            Wire::Record(record) => {
                keys.insert(record_key(&record));
                records.push(record);
            }
            _ => return Err(ProtocolError::Unexpected.into()),
        }
    }
    let mut labels = BTreeMap::new();
    for _ in 0..label_count {
        match channel.receive()? {
            Wire::RecordLabel(label) => {
                let key = (label.kind, label.path);
                if keys.contains(&key) {
                    if let Some(cleaned) = clean_label(&label.label) {
                        labels.insert(key, cleaned);
                    }
                }
            }
            _ => return Err(ProtocolError::Unexpected.into()),
        }
    }
    let reading = receive_data(channel, reading_bytes)?;
    match channel.receive()? {
        Wire::SnapshotEnd => Ok(Snapshot {
            records,
            labels,
            reading,
        }),
        _ => Err(ProtocolError::Unexpected.into()),
    }
}

pub(super) fn pull(
    channel: &mut NoiseChannel,
    storage: &PeerStorage,
    status: &SharedStatus,
    record: &ObjectRecord,
    label: &str,
) -> Result<(), PeerError> {
    channel.send(&Wire::Get {
        kind: record.kind.clone(),
        path: record.path.clone(),
    })?;
    match channel.receive()? {
        Wire::PutStart(received) if received == *record => {
            receive_file(channel, storage, status, received, label)
        }
        _ => Err(ProtocolError::Unexpected.into()),
    }
}

pub(super) fn push(
    channel: &mut NoiseChannel,
    storage: &PeerStorage,
    status: &SharedStatus,
    record: &ObjectRecord,
    label: &str,
) -> Result<(), PeerError> {
    channel.send(&Wire::PutStart(record.clone()))?;
    if record.deleted {
        status.sync_transfer(label, 0, "正在删除");
        let result = expect_applied(channel.receive()?);
        if result.is_ok() {
            status.sync_file_done();
        }
        result
    } else {
        send_file_body(channel, storage, status, record, label)
    }
}

pub(super) fn send_file(
    channel: &mut NoiseChannel,
    storage: &PeerStorage,
    status: &SharedStatus,
    record: &ObjectRecord,
    label: &str,
) -> Result<(), PeerError> {
    channel.send(&Wire::PutStart(record.clone()))?;
    send_file_body(channel, storage, status, record, label)
}

fn send_file_body(
    channel: &mut NoiseChannel,
    storage: &PeerStorage,
    status: &SharedStatus,
    record: &ObjectRecord,
    label: &str,
) -> Result<(), PeerError> {
    let offset = match channel.receive()? {
        Wire::Resume(offset) if offset <= record.size => offset,
        _ => return Err(ProtocolError::Unexpected.into()),
    };
    let mut file = storage.open_local(record)?;
    file.seek(SeekFrom::Start(offset))?;
    status.sync_transfer(label, record.size, "正在发送");
    status.progress(offset);
    let mut sent = offset;
    let mut buffer = vec![0_u8; CHUNK_BYTES];
    loop {
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        channel.send(&Wire::Data(buffer[..size].to_vec()))?;
        sent += size as u64;
        expect_data_ack(channel.receive()?, sent)?;
        status.progress(sent);
    }
    channel.send(&Wire::FileEnd)?;
    let result = expect_applied(channel.receive()?);
    if result.is_ok() {
        status.sync_file_done();
    }
    result
}

pub(super) fn receive_file(
    channel: &mut NoiseChannel,
    storage: &PeerStorage,
    status: &SharedStatus,
    record: ObjectRecord,
    label: &str,
) -> Result<(), PeerError> {
    let mut incoming = storage.begin_receive(&record)?;
    status.sync_transfer(label, record.size, "正在接收");
    status.progress(incoming.offset());
    channel.send(&Wire::Resume(incoming.offset()))?;
    loop {
        match channel.receive()? {
            Wire::Data(bytes) => {
                incoming.write_chunk(&bytes)?;
                let received = incoming.offset();
                channel.send(&Wire::DataAck(received))?;
                status.progress(received);
            }
            Wire::FileEnd => break,
            _ => return Err(ProtocolError::Unexpected.into()),
        }
    }
    storage.commit(incoming)?;
    channel.send(&Wire::Applied)?;
    status.sync_file_done();
    Ok(())
}

pub(super) fn send_reading(channel: &mut NoiseChannel, bytes: &[u8]) -> Result<(), PeerError> {
    channel.send(&Wire::ReadingStart(bytes.len() as u64))?;
    send_blob(channel, bytes)?;
    channel.send(&Wire::FileEnd)?;
    Ok(())
}

fn send_blob(channel: &mut NoiseChannel, bytes: &[u8]) -> Result<(), PeerError> {
    let mut sent = 0_u64;
    for chunk in bytes.chunks(CHUNK_BYTES) {
        channel.send(&Wire::Data(chunk.to_vec()))?;
        sent += chunk.len() as u64;
        expect_data_ack(channel.receive()?, sent)?;
    }
    Ok(())
}

pub(super) fn receive_blob(channel: &mut NoiseChannel, length: u64) -> Result<Vec<u8>, PeerError> {
    let bytes = receive_data(channel, length)?;
    match channel.receive()? {
        Wire::FileEnd => Ok(bytes),
        _ => Err(ProtocolError::Unexpected.into()),
    }
}

fn receive_data(channel: &mut NoiseChannel, length: u64) -> Result<Vec<u8>, PeerError> {
    if length > protocol::MAX_READING_BYTES as u64 {
        return Err(ProtocolError::SnapshotTooLarge.into());
    }
    let mut output = Vec::with_capacity(length as usize);
    while output.len() < length as usize {
        match channel.receive()? {
            Wire::Data(bytes)
                if !bytes.is_empty() && output.len() + bytes.len() <= length as usize =>
            {
                output.extend(bytes);
                channel.send(&Wire::DataAck(output.len() as u64))?;
            }
            _ => return Err(ProtocolError::Unexpected.into()),
        }
    }
    Ok(output)
}

pub(super) fn expect_applied(value: Wire) -> Result<(), PeerError> {
    match value {
        Wire::Applied => Ok(()),
        Wire::Error(message) => Err(PeerError::Remote(message)),
        _ => Err(ProtocolError::Unexpected.into()),
    }
}

pub(super) fn expect_done(value: Wire) -> Result<(), PeerError> {
    match value {
        Wire::Done => Ok(()),
        _ => Err(ProtocolError::Unexpected.into()),
    }
}
