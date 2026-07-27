pub(crate) mod control;
mod discovery;
mod error;
mod noise;
mod protocol;
pub(crate) mod reading;
pub(crate) mod storage;
mod summary;
mod transport;

pub use discovery::{DiscoveredPeer, DiscoveryService};
pub use error::PeerError;
pub use noise::MAGIC;
pub use summary::SyncSummary;

use crate::catalog::{now_ms, Catalog, DeviceIdentity, ObjectRecord, TrustedPeer};
use crate::server::SharedStatus;
use crate::sync_scope::{SyncItem, SyncSelection};
use control::ControlClient;
use discovery::pairing_code;
use noise::NoiseChannel;
use protocol::{
    expect_data_ack, expect_hello, validate_clock, ProtocolError, Snapshot, Wire, CHUNK_BYTES,
};
use reading::{merge as merge_reading, ReadingProvider, ReadingScope};
use std::io::{Read, Seek, SeekFrom};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use storage::PeerStorage;
use summary::{winner, Winner};
use transport::{configure, connect_any};

pub struct PeerRuntime {
    catalog: Arc<Catalog>,
    storage: PeerStorage,
    reading: ReadingProvider,
    control: ControlClient,
    status: Arc<SharedStatus>,
    active: AtomicBool,
}

impl PeerRuntime {
    pub fn new(
        catalog: Arc<Catalog>,
        storage: PeerStorage,
        reading: ReadingProvider,
        control: ControlClient,
        status: Arc<SharedStatus>,
    ) -> Self {
        Self {
            catalog,
            storage,
            reading,
            control,
            status,
            active: AtomicBool::new(false),
        }
    }

    pub fn is_trusted(&self, peer: &DiscoveredPeer) -> Result<bool, PeerError> {
        Ok(self
            .catalog
            .trusted_peer(&peer.id)?
            .is_some_and(|trusted| trusted.public_key == peer.public_key))
    }

    pub fn trust(&self, peer: &DiscoveredPeer) -> Result<(), PeerError> {
        self.catalog.trust_peer(&TrustedPeer {
            id: peer.id.clone(),
            name: peer.name.clone(),
            public_key: peer.public_key.clone(),
        })?;
        self.status.message(&format!(
            "本机已确认配对码 {}，请在另一台设备确认",
            peer.pairing_code
        ));
        Ok(())
    }

    pub fn synchronize(
        &self,
        peer: &DiscoveredPeer,
        selection: SyncSelection,
    ) -> Result<SyncSummary, PeerError> {
        let _guard = self.begin(&peer.name)?;
        if !selection.any() {
            return Err(PeerError::EmptySelection);
        }
        if !self.is_trusted(peer)? {
            return Err(PeerError::PairingRequired(peer.pairing_code.clone()));
        }
        let stream = connect_any(&peer.addresses)?;
        configure(&stream)?;
        let mut channel = NoiseChannel::initiate(stream, &self.catalog.identity().private_key)?;
        if channel.remote_static() != peer.public_key {
            return Err(PeerError::IdentityMismatch);
        }
        send_hello(&mut channel, self.catalog.identity())?;
        let (remote_id, remote_name, remote_time) = expect_hello(channel.receive()?)?;
        self.authenticate(
            &remote_id,
            &remote_name,
            remote_time,
            channel.remote_static(),
        )?;

        send_scope(&mut channel, &selection)?;
        let local = self.snapshot(&selection)?;
        send_snapshot(&mut channel, &local)?;
        let remote = receive_snapshot(&mut channel)?;
        let mut summary = SyncSummary::default();
        for (local_record, remote_record) in PeerStorage::winners(&local.records, &remote.records) {
            match winner(&local_record, &remote_record) {
                Winner::Remote(record) => {
                    if record.deleted {
                        self.storage.apply_tombstone(record)?;
                        summary.deleted += 1;
                    } else {
                        pull(&mut channel, &self.storage, &self.status, record)?;
                        summary.received += 1;
                    }
                }
                Winner::Local(record) => {
                    push(&mut channel, &self.storage, &self.status, record)?;
                    if record.deleted {
                        summary.deleted += 1;
                    } else {
                        summary.sent += 1;
                    }
                }
                Winner::Equal => {}
            }
        }
        let reading_scope = reading_scope(&selection);
        if reading_scope.any() {
            let merged = merge_reading(&local.reading, &remote.reading)?;
            send_reading(&mut channel, &merged)?;
            expect_applied(channel.receive()?)?;
            self.reading.import(&merged, reading_scope)?;
        }
        channel.send(&Wire::Done)?;
        expect_done(channel.receive()?)?;
        let items = selection.summaries().join("、");
        self.status.message(&format!(
            "同步完成：{}；接收 {}，发送 {}，删除 {}",
            items, summary.received, summary.sent, summary.deleted
        ));
        Ok(summary)
    }

    pub fn accept(&self, stream: TcpStream) -> Result<(), PeerError> {
        let _guard = self.begin("另一台设备")?;
        configure(&stream)?;
        let mut channel = NoiseChannel::accept(stream, &self.catalog.identity().private_key)?;
        let (remote_id, remote_name, remote_time) = expect_hello(channel.receive()?)?;
        self.authenticate(
            &remote_id,
            &remote_name,
            remote_time,
            channel.remote_static(),
        )?;
        send_hello(&mut channel, self.catalog.identity())?;
        let selection = receive_scope(&mut channel)?;
        if !selection.any() {
            return Err(PeerError::EmptySelection);
        }
        let remote = receive_snapshot(&mut channel)?;
        let local = self.snapshot(&selection)?;
        send_snapshot(&mut channel, &local)?;
        self.serve_commands(&mut channel, &remote.reading, &selection)?;
        self.status.message(&format!("已与 {remote_name} 完成同步"));
        Ok(())
    }

    fn serve_commands(
        &self,
        channel: &mut NoiseChannel,
        remote_reading: &[u8],
        selection: &SyncSelection,
    ) -> Result<(), PeerError> {
        loop {
            match channel.receive()? {
                Wire::Get { kind, path } if self.storage.kind_in_scope(&kind, selection) => {
                    let record = self
                        .catalog
                        .find(&kind, &path)?
                        .filter(|record| !record.deleted)
                        .ok_or(PeerError::MissingObject)?;
                    send_file(channel, &self.storage, &self.status, &record)?;
                }
                Wire::PutStart(record)
                    if record.deleted && self.storage.kind_in_scope(&record.kind, selection) =>
                {
                    self.storage.apply_tombstone(&record)?;
                    channel.send(&Wire::Applied)?;
                }
                Wire::PutStart(record) if self.storage.kind_in_scope(&record.kind, selection) => {
                    receive_file(channel, &self.storage, &self.status, record)?
                }
                Wire::ReadingStart(length) if reading_scope(selection).any() => {
                    let received = receive_blob(channel, length)?;
                    let scope = reading_scope(selection);
                    let merged = merge_reading(&self.reading.export(scope)?, remote_reading)?;
                    let final_state = merge_reading(&merged, &received)?;
                    self.reading.import(&final_state, scope)?;
                    channel.send(&Wire::Applied)?;
                }
                Wire::Get { .. } | Wire::PutStart(_) | Wire::ReadingStart(_) => {
                    channel.send(&Wire::Error("同步项未被本次会话选中".into()))?;
                    return Err(ProtocolError::Unexpected.into());
                }
                Wire::Done => {
                    channel.send(&Wire::Done)?;
                    return Ok(());
                }
                Wire::Error(message) => return Err(PeerError::Remote(message)),
                _ => return Err(ProtocolError::Unexpected.into()),
            }
        }
    }

    fn snapshot(&self, selection: &SyncSelection) -> Result<Snapshot, PeerError> {
        self.quiesce(selection)?;
        let scope = reading_scope(selection);
        let reading = if scope.any() {
            self.reading.export(scope)?
        } else {
            Vec::new()
        };
        let records = self.storage.scan(selection)?;
        Ok(Snapshot { records, reading })
    }

    fn quiesce(&self, selection: &SyncSelection) -> Result<(), PeerError> {
        if selection.contains(SyncItem::Magicpaper) {
            self.status.message("正在关闭 MagicPaper 后台服务");
            self.control.close_complete("magicpaper")?;
        }
        Ok(())
    }

    fn authenticate(
        &self,
        id: &str,
        name: &str,
        time_ms: i64,
        public_key: &[u8],
    ) -> Result<(), PeerError> {
        validate_clock(now_ms(), time_ms)?;
        let expected_id = hex::encode(&blake3::hash(public_key).as_bytes()[..8]);
        let trusted = self.catalog.trusted_peer(id)?;
        if expected_id != id
            || !trusted.is_some_and(|peer| peer.public_key == public_key && peer.name == name)
        {
            let code = pairing_code(&self.catalog.identity().public_key, public_key);
            self.status
                .message(&format!("需要先在两台设备确认配对码 {code}"));
            return Err(PeerError::PairingRequired(code));
        }
        Ok(())
    }

    fn begin(&self, label: &str) -> Result<ActiveGuard<'_>, PeerError> {
        if self
            .active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(PeerError::Busy);
        }
        self.status.activity(label, "正在同步");
        Ok(ActiveGuard(&self.active))
    }
}

fn send_hello(channel: &mut NoiseChannel, identity: &DeviceIdentity) -> Result<(), PeerError> {
    channel.send(&Wire::Hello {
        schema: protocol::PROTOCOL_SCHEMA,
        id: identity.id.clone(),
        name: identity.name.clone(),
        time_ms: now_ms(),
    })?;
    Ok(())
}

fn send_scope(channel: &mut NoiseChannel, selection: &SyncSelection) -> Result<(), PeerError> {
    channel.send(&Wire::Scope(selection.clone()))?;
    Ok(())
}

fn receive_scope(channel: &mut NoiseChannel) -> Result<SyncSelection, PeerError> {
    match channel.receive()? {
        Wire::Scope(selection) => Ok(selection),
        _ => Err(ProtocolError::Unexpected.into()),
    }
}

fn reading_scope(selection: &SyncSelection) -> ReadingScope {
    ReadingScope {
        reading: selection.contains(SyncItem::Koreader),
        font_size: selection.contains(SyncItem::KoreaderFontSize),
    }
}

fn send_snapshot(channel: &mut NoiseChannel, snapshot: &Snapshot) -> Result<(), PeerError> {
    channel.send(&Wire::SnapshotStart {
        records: snapshot
            .records
            .len()
            .try_into()
            .map_err(|_| ProtocolError::SnapshotTooLarge)?,
        reading_bytes: snapshot.reading.len() as u64,
    })?;
    for record in &snapshot.records {
        channel.send(&Wire::Record(record.clone()))?;
    }
    send_blob(channel, &snapshot.reading)?;
    channel.send(&Wire::SnapshotEnd)?;
    Ok(())
}

fn receive_snapshot(channel: &mut NoiseChannel) -> Result<Snapshot, PeerError> {
    let (count, reading_bytes) = match channel.receive()? {
        Wire::SnapshotStart {
            records,
            reading_bytes,
        } if records <= 100_000 && reading_bytes <= protocol::MAX_READING_BYTES as u64 => {
            (records, reading_bytes)
        }
        _ => return Err(ProtocolError::SnapshotTooLarge.into()),
    };
    let mut records = Vec::with_capacity(count as usize);
    for _ in 0..count {
        match channel.receive()? {
            Wire::Record(record) => records.push(record),
            _ => return Err(ProtocolError::Unexpected.into()),
        }
    }
    let reading = receive_data(channel, reading_bytes)?;
    match channel.receive()? {
        Wire::SnapshotEnd => Ok(Snapshot { records, reading }),
        _ => Err(ProtocolError::Unexpected.into()),
    }
}

fn pull(
    channel: &mut NoiseChannel,
    storage: &PeerStorage,
    status: &SharedStatus,
    record: &ObjectRecord,
) -> Result<(), PeerError> {
    channel.send(&Wire::Get {
        kind: record.kind.clone(),
        path: record.path.clone(),
    })?;
    match channel.receive()? {
        Wire::PutStart(received) if received == *record => {
            receive_file(channel, storage, status, received)
        }
        _ => Err(ProtocolError::Unexpected.into()),
    }
}

fn push(
    channel: &mut NoiseChannel,
    storage: &PeerStorage,
    status: &SharedStatus,
    record: &ObjectRecord,
) -> Result<(), PeerError> {
    channel.send(&Wire::PutStart(record.clone()))?;
    if record.deleted {
        expect_applied(channel.receive()?)
    } else {
        send_file_body(channel, storage, status, record)
    }
}

fn send_file(
    channel: &mut NoiseChannel,
    storage: &PeerStorage,
    status: &SharedStatus,
    record: &ObjectRecord,
) -> Result<(), PeerError> {
    channel.send(&Wire::PutStart(record.clone()))?;
    send_file_body(channel, storage, status, record)
}

fn send_file_body(
    channel: &mut NoiseChannel,
    storage: &PeerStorage,
    status: &SharedStatus,
    record: &ObjectRecord,
) -> Result<(), PeerError> {
    let offset = match channel.receive()? {
        Wire::Resume(offset) if offset <= record.size => offset,
        _ => return Err(ProtocolError::Unexpected.into()),
    };
    let mut file = storage.open_local(record)?;
    file.seek(SeekFrom::Start(offset))?;
    status.transfer(&record.path, record.size, "正在发送");
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
    expect_applied(channel.receive()?)
}

fn receive_file(
    channel: &mut NoiseChannel,
    storage: &PeerStorage,
    status: &SharedStatus,
    record: ObjectRecord,
) -> Result<(), PeerError> {
    let mut incoming = storage.begin_receive(&record)?;
    status.transfer(&record.path, record.size, "正在接收");
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
    Ok(())
}

fn send_reading(channel: &mut NoiseChannel, bytes: &[u8]) -> Result<(), PeerError> {
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

fn receive_blob(channel: &mut NoiseChannel, length: u64) -> Result<Vec<u8>, PeerError> {
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

fn expect_applied(value: Wire) -> Result<(), PeerError> {
    match value {
        Wire::Applied => Ok(()),
        Wire::Error(message) => Err(PeerError::Remote(message)),
        _ => Err(ProtocolError::Unexpected.into()),
    }
}

fn expect_done(value: Wire) -> Result<(), PeerError> {
    match value {
        Wire::Done => Ok(()),
        _ => Err(ProtocolError::Unexpected.into()),
    }
}

struct ActiveGuard<'a>(&'a AtomicBool);
impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
