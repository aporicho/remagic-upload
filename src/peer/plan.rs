use crate::catalog::ObjectRecord;

use super::protocol::Snapshot;
use super::storage::PeerStorage;
use super::summary::{winner, Winner};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncDirection {
    Send,
    Receive,
    Delete,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SyncOperation {
    pub record: ObjectRecord,
    pub label: String,
    pub direction: SyncDirection,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SyncPlan {
    operations: Vec<SyncOperation>,
    send_files: usize,
    receive_files: usize,
    delete_files: usize,
    total_bytes: u64,
}

impl SyncPlan {
    pub fn for_initiator(local: &Snapshot, remote: &Snapshot) -> Self {
        Self::build(local, remote, PlanRole::Initiator)
    }

    pub fn for_acceptor(local: &Snapshot, remote: &Snapshot) -> Self {
        Self::build(local, remote, PlanRole::Acceptor)
    }

    pub fn send_files(&self) -> usize {
        self.send_files
    }

    pub fn receive_files(&self) -> usize {
        self.receive_files
    }

    pub fn delete_files(&self) -> usize {
        self.delete_files
    }

    #[cfg(test)]
    pub fn total_files(&self) -> usize {
        self.operations.len()
    }

    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    pub fn preview(&self) -> Vec<String> {
        self.operations
            .iter()
            .take(6)
            .map(|operation| operation.label.clone())
            .collect()
    }

    pub fn label_for(&self, record: &ObjectRecord, direction: SyncDirection) -> String {
        self.operations
            .iter()
            .find(|operation| {
                operation.direction == direction
                    && operation.record.kind == record.kind
                    && operation.record.path == record.path
            })
            .map(|operation| operation.label.clone())
            .unwrap_or_else(|| record.path.clone())
    }

    fn build(local: &Snapshot, remote: &Snapshot, role: PlanRole) -> Self {
        let mut plan = Self::default();
        for (local_record, remote_record) in PeerStorage::winners(&local.records, &remote.records) {
            match winner(&local_record, &remote_record) {
                Winner::Remote(record) => {
                    let direction = if record.deleted {
                        SyncDirection::Delete
                    } else {
                        SyncDirection::Receive
                    };
                    plan.push(record.clone(), remote.label_for(record), direction);
                }
                Winner::Local(record) => {
                    if record.deleted && role == PlanRole::Acceptor {
                        continue;
                    }
                    let direction = if record.deleted {
                        SyncDirection::Delete
                    } else {
                        SyncDirection::Send
                    };
                    plan.push(record.clone(), local.label_for(record), direction);
                }
                Winner::Equal => {}
            }
        }
        plan
    }

    fn push(&mut self, record: ObjectRecord, label: String, direction: SyncDirection) {
        match direction {
            SyncDirection::Send => {
                self.send_files += 1;
                self.total_bytes = self.total_bytes.saturating_add(record.size);
            }
            SyncDirection::Receive => {
                self.receive_files += 1;
                self.total_bytes = self.total_bytes.saturating_add(record.size);
            }
            SyncDirection::Delete => {
                self.delete_files += 1;
            }
        }
        self.operations.push(SyncOperation {
            record,
            label,
            direction,
        });
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PlanRole {
    Initiator,
    Acceptor,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::catalog::VersionStamp;

    use super::*;

    fn record(path: &str, counter: u64, deleted: bool, size: u64) -> ObjectRecord {
        ObjectRecord {
            kind: "xochitl_document".into(),
            path: path.into(),
            size,
            hash: format!("{counter:064x}"),
            mode: 0o644,
            version: VersionStamp {
                wall_time_ms: counter as i64,
                counter,
                origin: "0123456789abcdef".into(),
            },
            deleted,
        }
    }

    fn snapshot(records: Vec<ObjectRecord>, labels: &[(&str, &str)]) -> Snapshot {
        Snapshot {
            records,
            labels: labels
                .iter()
                .map(|(path, label)| {
                    (
                        ("xochitl_document".to_owned(), (*path).to_owned()),
                        (*label).to_owned(),
                    )
                })
                .collect::<BTreeMap<_, _>>(),
            reading: Vec::new(),
        }
    }

    #[test]
    fn initiator_counts_send_receive_and_delete_operations() {
        let send = record("send.epub", 3, false, 10);
        let receive = record("receive.pdf", 4, false, 20);
        let delete = record("delete.metadata", 5, true, 1);
        let local = snapshot(
            vec![send.clone(), delete.clone()],
            &[
                ("send.epub", "发出.epub"),
                ("delete.metadata", "删除.metadata"),
            ],
        );
        let remote = snapshot(vec![receive.clone()], &[("receive.pdf", "接收.pdf")]);

        let plan = SyncPlan::for_initiator(&local, &remote);

        assert_eq!(plan.send_files(), 1);
        assert_eq!(plan.receive_files(), 1);
        assert_eq!(plan.delete_files(), 1);
        assert_eq!(plan.total_files(), 3);
        assert_eq!(plan.total_bytes(), 30);
        assert_eq!(
            plan.preview(),
            vec![
                "删除.metadata".to_owned(),
                "接收.pdf".to_owned(),
                "发出.epub".to_owned()
            ]
        );
    }

    #[test]
    fn acceptor_does_not_wait_for_its_own_tombstone() {
        let deleted_local = record("gone.pdf", 5, true, 30);
        let local = snapshot(vec![deleted_local], &[("gone.pdf", "已删除.pdf")]);
        let remote = snapshot(Vec::new(), &[]);

        let plan = SyncPlan::for_acceptor(&local, &remote);

        assert_eq!(plan.total_files(), 0);
        assert_eq!(plan.delete_files(), 0);
    }
}
