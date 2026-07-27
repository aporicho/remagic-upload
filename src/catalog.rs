use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

mod files;
mod identity;
#[cfg(test)]
mod tests;
mod validate;

#[cfg(test)]
use files::supported_object_path;
use files::{collect_files, file_mode, hash_file, modified_ms, normalized_relative};
use identity::load_or_create_identity;
use validate::{create_private_directory, validate_kind, validate_peer, validate_record};

#[derive(Clone, Debug)]
pub struct DeviceIdentity {
    pub id: String,
    pub name: String,
    pub private_key: Vec<u8>,
    pub public_key: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VersionStamp {
    pub wall_time_ms: i64,
    pub counter: u64,
    pub origin: String,
}

impl Ord for VersionStamp {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.wall_time_ms, self.counter, &self.origin).cmp(&(
            other.wall_time_ms,
            other.counter,
            &other.origin,
        ))
    }
}

impl PartialOrd for VersionStamp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ObjectRecord {
    pub kind: String,
    pub path: String,
    pub size: u64,
    pub hash: String,
    pub mode: u32,
    pub version: VersionStamp,
    pub deleted: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustedPeer {
    pub id: String,
    pub name: String,
    pub public_key: Vec<u8>,
}

pub struct Catalog {
    connection: Mutex<Connection>,
    identity: DeviceIdentity,
}

impl Catalog {
    pub fn open(data_home: &Path, device_name: &str) -> Result<Self, CatalogError> {
        create_private_directory(data_home)?;
        let path = data_home.join("sync-v1.sqlite3");
        let mut connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.execute_batch(
            "BEGIN;
             CREATE TABLE IF NOT EXISTS meta (
               key TEXT PRIMARY KEY NOT NULL,
               value BLOB NOT NULL
             );
             CREATE TABLE IF NOT EXISTS objects (
               kind TEXT NOT NULL,
               path TEXT NOT NULL,
               size INTEGER NOT NULL,
               hash TEXT NOT NULL,
               mode INTEGER NOT NULL DEFAULT 420,
               mtime_ms INTEGER NOT NULL,
               wall_time_ms INTEGER NOT NULL,
               counter INTEGER NOT NULL,
               origin TEXT NOT NULL,
               deleted INTEGER NOT NULL CHECK (deleted IN (0, 1)),
               PRIMARY KEY(kind, path)
             );
             CREATE TABLE IF NOT EXISTS peers (
               id TEXT PRIMARY KEY NOT NULL,
               name TEXT NOT NULL,
               public_key BLOB NOT NULL,
               last_seen_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS transfer_parts (
               peer_id TEXT NOT NULL,
               kind TEXT NOT NULL,
               path TEXT NOT NULL,
               version TEXT NOT NULL,
               received INTEGER NOT NULL,
               temporary_path TEXT NOT NULL,
               PRIMARY KEY(peer_id, kind, path)
             );
             COMMIT;",
        )?;
        ensure_object_mode_column(&connection)?;
        let identity = load_or_create_identity(&mut connection, device_name)?;
        Ok(Self {
            connection: Mutex::new(connection),
            identity,
        })
    }

    pub fn identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    pub fn next_version(&self) -> Result<VersionStamp, CatalogError> {
        let mut connection = self.connection.lock().map_err(|_| CatalogError::Poisoned)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let counter = meta_u64(&transaction, "counter")?.saturating_add(1);
        set_meta(&transaction, "counter", counter.to_string().as_bytes())?;
        transaction.commit()?;
        Ok(VersionStamp {
            wall_time_ms: now_ms(),
            counter,
            origin: self.identity.id.clone(),
        })
    }

    #[cfg(test)]
    pub fn scan(&self, kind: &str, root: &Path) -> Result<Vec<ObjectRecord>, CatalogError> {
        self.scan_filtered(kind, root, |relative| supported_object_path(kind, relative))
    }

    pub fn scan_filtered<F>(
        &self,
        kind: &str,
        root: &Path,
        accepts: F,
    ) -> Result<Vec<ObjectRecord>, CatalogError>
    where
        F: Fn(&str) -> bool,
    {
        validate_kind(kind)?;
        let mut files = Vec::new();
        match fs::symlink_metadata(root) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(CatalogError::UnsafeFile(root.to_path_buf()));
            }
            Ok(_) => collect_files(root, root, &mut files)?,
        }
        files.retain(|(relative, _, _)| accepts(relative));
        let mut connection = self.connection.lock().map_err(|_| CatalogError::Poisoned)?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut seen = std::collections::BTreeSet::new();
        for (relative, absolute, metadata) in files {
            seen.insert(relative.clone());
            let size = metadata.len();
            let mode = file_mode(&metadata);
            let mtime_ms = modified_ms(&metadata);
            let existing = select_object(&transaction, kind, &relative)?;
            if existing.as_ref().is_some_and(|record| {
                !record.object.deleted
                    && record.object.size == size
                    && record.object.mode == mode
                    && record.mtime_ms == mtime_ms
            }) {
                continue;
            }
            let hash = hash_file(&absolute)?;
            if existing.as_ref().is_some_and(|record| {
                !record.object.deleted && record.object.hash == hash && record.object.mode == mode
            }) {
                let object = existing.expect("checked");
                upsert_object(&transaction, &object.object, mtime_ms)?;
                continue;
            }
            let version = next_version_in(&transaction, &self.identity.id, mtime_ms)?;
            upsert_object(
                &transaction,
                &ObjectRecord {
                    kind: kind.to_owned(),
                    path: relative,
                    size,
                    hash,
                    mode,
                    version,
                    deleted: false,
                },
                mtime_ms,
            )?;
        }

        let live_paths = list_kind(&transaction, kind)?
            .into_iter()
            .filter(|record| !record.deleted && accepts(&record.path))
            .map(|record| record.path)
            .collect::<Vec<_>>();
        for path in live_paths {
            if seen.contains(&path) {
                continue;
            }
            let version = next_version_in(&transaction, &self.identity.id, now_ms())?;
            let previous = select_object(&transaction, kind, &path)?
                .ok_or_else(|| CatalogError::MissingObject(path.clone()))?;
            upsert_object(
                &transaction,
                &ObjectRecord {
                    kind: kind.to_owned(),
                    path,
                    size: previous.object.size,
                    hash: previous.object.hash,
                    mode: previous.object.mode,
                    version,
                    deleted: true,
                },
                0,
            )?;
        }
        let records = list_kind(&transaction, kind)?
            .into_iter()
            .filter(|record| accepts(&record.path))
            .collect();
        transaction.commit()?;
        Ok(records)
    }

    pub fn record_local_file(
        &self,
        kind: &str,
        root: &Path,
        path: &Path,
    ) -> Result<ObjectRecord, CatalogError> {
        let relative = normalized_relative(root, path)?;
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(CatalogError::UnsafeFile(path.to_path_buf()));
        }
        let record = ObjectRecord {
            kind: kind.to_owned(),
            path: relative,
            size: metadata.len(),
            hash: hash_file(path)?,
            mode: file_mode(&metadata),
            version: self.next_version()?,
            deleted: false,
        };
        self.store_remote(&record, modified_ms(&metadata))?;
        Ok(record)
    }

    pub fn store_remote(&self, record: &ObjectRecord, mtime_ms: i64) -> Result<(), CatalogError> {
        validate_record(record)?;
        let connection = self.connection.lock().map_err(|_| CatalogError::Poisoned)?;
        upsert_object(&connection, record, mtime_ms)?;
        Ok(())
    }

    pub fn find(&self, kind: &str, path: &str) -> Result<Option<ObjectRecord>, CatalogError> {
        let connection = self.connection.lock().map_err(|_| CatalogError::Poisoned)?;
        Ok(select_object(&connection, kind, path)?.map(|value| value.object))
    }

    pub fn trust_peer(&self, peer: &TrustedPeer) -> Result<(), CatalogError> {
        validate_peer(peer)?;
        let connection = self.connection.lock().map_err(|_| CatalogError::Poisoned)?;
        connection.execute(
            "INSERT INTO peers(id,name,public_key,last_seen_ms) VALUES(?1,?2,?3,?4)
             ON CONFLICT(id) DO UPDATE SET name=excluded.name,
               public_key=excluded.public_key,last_seen_ms=excluded.last_seen_ms",
            params![peer.id, peer.name, peer.public_key, now_ms()],
        )?;
        Ok(())
    }

    pub fn trusted_peer(&self, id: &str) -> Result<Option<TrustedPeer>, CatalogError> {
        let connection = self.connection.lock().map_err(|_| CatalogError::Poisoned)?;
        Ok(connection
            .query_row(
                "SELECT id,name,public_key FROM peers WHERE id=?1",
                [id],
                |row| {
                    Ok(TrustedPeer {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        public_key: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }
}

struct StoredObject {
    object: ObjectRecord,
    mtime_ms: i64,
}

fn meta(connection: &Connection, key: &str) -> Result<Option<Vec<u8>>, rusqlite::Error> {
    connection
        .query_row("SELECT value FROM meta WHERE key=?1", [key], |row| {
            row.get(0)
        })
        .optional()
}

fn meta_u64(connection: &Connection, key: &str) -> Result<u64, CatalogError> {
    let bytes = meta(connection, key)?.unwrap_or_else(|| b"0".to_vec());
    let text = std::str::from_utf8(&bytes).map_err(|_| CatalogError::CorruptCounter)?;
    text.parse().map_err(|_| CatalogError::CorruptCounter)
}

fn set_meta(connection: &Connection, key: &str, value: &[u8]) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT INTO meta(key,value) VALUES(?1,?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn ensure_object_mode_column(connection: &Connection) -> Result<(), rusqlite::Error> {
    let mut statement = connection.prepare("PRAGMA table_info(objects)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !columns.iter().any(|column| column == "mode") {
        connection.execute(
            "ALTER TABLE objects ADD COLUMN mode INTEGER NOT NULL DEFAULT 420",
            [],
        )?;
    }
    Ok(())
}

fn next_version_in(
    connection: &Connection,
    origin: &str,
    baseline_ms: i64,
) -> Result<VersionStamp, CatalogError> {
    let counter = meta_u64(connection, "counter")?.saturating_add(1);
    set_meta(connection, "counter", counter.to_string().as_bytes())?;
    Ok(VersionStamp {
        wall_time_ms: now_ms().max(baseline_ms),
        counter,
        origin: origin.to_owned(),
    })
}

fn select_object(
    connection: &Connection,
    kind: &str,
    path: &str,
) -> Result<Option<StoredObject>, rusqlite::Error> {
    connection
        .query_row(
            "SELECT kind,path,size,hash,mode,wall_time_ms,counter,origin,deleted,mtime_ms
             FROM objects WHERE kind=?1 AND path=?2",
            params![kind, path],
            |row| {
                Ok(StoredObject {
                    object: row_object(row)?,
                    mtime_ms: row.get(9)?,
                })
            },
        )
        .optional()
}

fn row_object(row: &rusqlite::Row<'_>) -> Result<ObjectRecord, rusqlite::Error> {
    Ok(ObjectRecord {
        kind: row.get(0)?,
        path: row.get(1)?,
        size: row.get::<_, i64>(2)? as u64,
        hash: row.get(3)?,
        mode: row.get::<_, i64>(4)? as u32,
        version: VersionStamp {
            wall_time_ms: row.get(5)?,
            counter: row.get::<_, i64>(6)? as u64,
            origin: row.get(7)?,
        },
        deleted: row.get::<_, i64>(8)? != 0,
    })
}

fn list_kind(connection: &Connection, kind: &str) -> Result<Vec<ObjectRecord>, rusqlite::Error> {
    let mut statement = connection.prepare(
        "SELECT kind,path,size,hash,mode,wall_time_ms,counter,origin,deleted
         FROM objects WHERE kind=?1 ORDER BY path",
    )?;
    let records = statement
        .query_map([kind], row_object)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(records)
}

fn upsert_object(
    connection: &Connection,
    record: &ObjectRecord,
    mtime_ms: i64,
) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT INTO objects(kind,path,size,hash,mode,mtime_ms,wall_time_ms,counter,origin,deleted)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
         ON CONFLICT(kind,path) DO UPDATE SET size=excluded.size,hash=excluded.hash,
           mode=excluded.mode,mtime_ms=excluded.mtime_ms,wall_time_ms=excluded.wall_time_ms,
           counter=excluded.counter,origin=excluded.origin,deleted=excluded.deleted",
        params![
            record.kind,
            record.path,
            record.size as i64,
            record.hash,
            record.mode as i64,
            mtime_ms,
            record.version.wall_time_ms,
            record.version.counter as i64,
            record.version.origin,
            i64::from(record.deleted),
        ],
    )?;
    Ok(())
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

#[derive(Debug, Error)]
pub enum CatalogError {
    #[error("catalog mutex is poisoned")]
    Poisoned,
    #[error("catalog identity is incomplete or corrupt")]
    CorruptIdentity,
    #[error("catalog version counter is corrupt")]
    CorruptCounter,
    #[error("invalid sync object kind: {0}")]
    InvalidKind(String),
    #[error("invalid sync object record: {0}")]
    InvalidRecord(String),
    #[error("invalid trusted peer")]
    InvalidPeer,
    #[error("unsafe file in synchronized storage: {0}")]
    UnsafeFile(PathBuf),
    #[error("catalog object disappeared: {0}")]
    MissingObject(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Noise(#[from] snow::Error),
}
