pub(crate) mod control;
mod discovery;
mod error;
mod noise;
mod plan;
mod protocol;
pub(crate) mod reading;
pub(crate) mod storage;
mod summary;
mod transfer;
mod transport;

pub use discovery::{DiscoveredPeer, DiscoveryService};
pub use error::PeerError;
pub use noise::MAGIC;
pub use summary::SyncSummary;

use crate::catalog::{now_ms, Catalog, DeviceIdentity, TrustedPeer};
use crate::server::SharedStatus;
use crate::sync_scope::{SyncItem, SyncSelection};
use control::ControlClient;
use discovery::pairing_code;
use noise::NoiseChannel;
use plan::{SyncDirection, SyncPlan};
use protocol::{expect_hello, validate_clock, ProtocolError, Snapshot, Wire};
use reading::{merge as merge_reading, ReadingProvider, ReadingScope};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use storage::PeerStorage;
use summary::{winner, Winner};
use transfer::{
    expect_applied, expect_done, pull, push, receive_blob, receive_file, receive_snapshot,
    send_file, send_reading, send_snapshot,
};
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
        let plan = SyncPlan::for_initiator(&local, &remote);
        self.status.sync_plan(
            plan.send_files(),
            plan.receive_files(),
            plan.delete_files(),
            plan.total_bytes(),
            plan.preview(),
        );
        let mut summary = SyncSummary::default();
        for (local_record, remote_record) in PeerStorage::winners(&local.records, &remote.records) {
            match winner(&local_record, &remote_record) {
                Winner::Remote(record) => {
                    if record.deleted {
                        let label = plan.label_for(record, SyncDirection::Delete);
                        self.status.sync_transfer(&label, 0, "正在删除");
                        self.storage.apply_tombstone(record)?;
                        self.status.sync_file_done();
                        summary.deleted += 1;
                    } else {
                        let label = plan.label_for(record, SyncDirection::Receive);
                        pull(&mut channel, &self.storage, &self.status, record, &label)?;
                        summary.received += 1;
                    }
                }
                Winner::Local(record) => {
                    let direction = if record.deleted {
                        SyncDirection::Delete
                    } else {
                        SyncDirection::Send
                    };
                    let label = plan.label_for(record, direction);
                    push(&mut channel, &self.storage, &self.status, record, &label)?;
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
        self.status.sync_complete(&format!(
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
        let plan = SyncPlan::for_acceptor(&local, &remote);
        self.status.sync_plan(
            plan.send_files(),
            plan.receive_files(),
            plan.delete_files(),
            plan.total_bytes(),
            plan.preview(),
        );
        self.serve_commands(&mut channel, &remote.reading, &selection, &plan)?;
        self.status
            .sync_complete(&format!("已与 {remote_name} 完成同步"));
        Ok(())
    }

    fn serve_commands(
        &self,
        channel: &mut NoiseChannel,
        remote_reading: &[u8],
        selection: &SyncSelection,
        plan: &SyncPlan,
    ) -> Result<(), PeerError> {
        loop {
            match channel.receive()? {
                Wire::Get { kind, path } if self.storage.kind_in_scope(&kind, selection) => {
                    let record = self
                        .catalog
                        .find(&kind, &path)?
                        .filter(|record| !record.deleted)
                        .ok_or(PeerError::MissingObject)?;
                    let label = plan.label_for(&record, SyncDirection::Send);
                    send_file(channel, &self.storage, &self.status, &record, &label)?;
                }
                Wire::PutStart(record)
                    if record.deleted && self.storage.kind_in_scope(&record.kind, selection) =>
                {
                    let label = plan.label_for(&record, SyncDirection::Delete);
                    self.status.sync_transfer(&label, 0, "正在删除");
                    self.storage.apply_tombstone(&record)?;
                    self.status.sync_file_done();
                    channel.send(&Wire::Applied)?;
                }
                Wire::PutStart(record) if self.storage.kind_in_scope(&record.kind, selection) => {
                    let label = plan.label_for(&record, SyncDirection::Receive);
                    receive_file(channel, &self.storage, &self.status, record, &label)?
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
        let labels = self.storage.labels(&records);
        Ok(Snapshot {
            records,
            labels,
            reading,
        })
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
        document_settings: selection.contains(SyncItem::KoreaderDocumentSettings),
    }
}

struct ActiveGuard<'a>(&'a AtomicBool);
impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
