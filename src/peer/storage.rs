use crate::catalog::{Catalog, ObjectRecord};
use crate::trash::Trash;
use crate::upload::validate::{validate_book, validate_png};
use blake3::Hasher;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

#[derive(Clone)]
pub struct PeerStorage {
    books: PathBuf,
    wallpapers: PathBuf,
    catalog: Arc<Catalog>,
    trash: Trash,
}

pub struct IncomingFile {
    record: ObjectRecord,
    temporary: PathBuf,
    output: File,
    received: u64,
}

impl PeerStorage {
    pub fn new(
        books: PathBuf,
        wallpapers: PathBuf,
        catalog: Arc<Catalog>,
        data_home: &Path,
    ) -> Result<Self, StorageError> {
        ensure_root(&books)?;
        ensure_root(&wallpapers)?;
        Ok(Self {
            books,
            wallpapers,
            catalog,
            trash: Trash::new(data_home)?,
        })
    }

    pub fn scan(&self) -> Result<Vec<ObjectRecord>, StorageError> {
        self.catalog.scan("book", &self.books)?;
        self.catalog.scan("wallpaper", &self.wallpapers)?;
        Ok(self.catalog.records()?)
    }

    pub fn open_local(&self, record: &ObjectRecord) -> Result<File, StorageError> {
        if record.deleted {
            return Err(StorageError::DeletedSource);
        }
        let path = self.path_for(record)?;
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(StorageError::UnsafePath(path));
        }
        Ok(File::open(path)?)
    }

    pub fn begin_receive(&self, record: &ObjectRecord) -> Result<IncomingFile, StorageError> {
        validate_record_path(record)?;
        if record.deleted {
            return Err(StorageError::DeletedSource);
        }
        let destination = self.path_for(record)?;
        fs::create_dir_all(
            destination
                .parent()
                .ok_or_else(|| StorageError::UnsafePath(destination.clone()))?,
        )?;
        let temporary = self.part_path(record)?;
        let mut output = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&temporary)?;
        let received = output.metadata()?.len();
        let received = if received <= record.size { received } else { 0 };
        if output.metadata()?.len() != received {
            output.set_len(0)?;
        }
        output.seek(SeekFrom::End(0))?;
        Ok(IncomingFile {
            record: record.clone(),
            temporary,
            output,
            received,
        })
    }

    pub fn commit(&self, mut incoming: IncomingFile) -> Result<(), StorageError> {
        incoming.output.flush()?;
        incoming.output.sync_all()?;
        if incoming.received != incoming.record.size {
            return Err(StorageError::Size {
                expected: incoming.record.size,
                actual: incoming.received,
            });
        }
        let hash = hash_file(&incoming.temporary)?;
        if hash != incoming.record.hash {
            let _ = fs::remove_file(&incoming.temporary);
            return Err(StorageError::Hash);
        }
        validate_format(&incoming.record, &incoming.temporary)?;
        let destination = self.path_for(&incoming.record)?;
        let relative = Path::new(&incoming.record.path);
        let previous = self
            .trash
            .move_existing(&incoming.record.kind, relative, &destination)?;
        if let Err(error) = fs::rename(&incoming.temporary, &destination) {
            if let Some(previous) = previous {
                let _ = self.trash.restore(&previous, &destination);
            }
            return Err(error.into());
        }
        if let Err(error) = self
            .catalog
            .store_remote(&incoming.record, modified_ms(&destination)?)
        {
            let _ = fs::remove_file(&destination);
            if let Some(previous) = previous {
                let _ = self.trash.restore(&previous, &destination);
            }
            return Err(error.into());
        }
        sync_parent(&destination)?;
        let _ = self.trash.prune_expired();
        Ok(())
    }

    pub fn apply_tombstone(&self, record: &ObjectRecord) -> Result<(), StorageError> {
        validate_record_path(record)?;
        if !record.deleted {
            return Err(StorageError::NotDeleted);
        }
        let destination = self.path_for(record)?;
        self.trash
            .move_existing(&record.kind, Path::new(&record.path), &destination)?;
        self.catalog.store_remote(record, 0)?;
        let _ = self.trash.prune_expired();
        Ok(())
    }

    pub fn winners(
        local: &[ObjectRecord],
        remote: &[ObjectRecord],
    ) -> Vec<(Option<ObjectRecord>, Option<ObjectRecord>)> {
        let mut map =
            BTreeMap::<(String, String), (Option<ObjectRecord>, Option<ObjectRecord>)>::new();
        for record in local {
            map.entry((record.kind.clone(), record.path.clone()))
                .or_default()
                .0 = Some(record.clone());
        }
        for record in remote {
            map.entry((record.kind.clone(), record.path.clone()))
                .or_default()
                .1 = Some(record.clone());
        }
        map.into_values().collect()
    }

    fn path_for(&self, record: &ObjectRecord) -> Result<PathBuf, StorageError> {
        validate_record_path(record)?;
        let root = match record.kind.as_str() {
            "book" => &self.books,
            "wallpaper" => &self.wallpapers,
            _ => return Err(StorageError::InvalidKind),
        };
        Ok(root.join(&record.path))
    }

    fn part_path(&self, record: &ObjectRecord) -> Result<PathBuf, StorageError> {
        let destination = self.path_for(record)?;
        let token = &blake3::hash(
            format!("{}:{}:{}", record.kind, record.path, record.version.counter).as_bytes(),
        )
        .to_hex()[..16];
        Ok(destination
            .parent()
            .ok_or(StorageError::UnsafePath(destination.clone()))?
            .join(format!(".remagic-sync-{token}.part")))
    }
}

impl IncomingFile {
    pub fn offset(&self) -> u64 {
        self.received
    }

    pub fn write_chunk(&mut self, bytes: &[u8]) -> Result<(), StorageError> {
        let next = self.received.saturating_add(bytes.len() as u64);
        if bytes.is_empty() || next > self.record.size {
            return Err(StorageError::Size {
                expected: self.record.size,
                actual: next,
            });
        }
        self.output.write_all(bytes)?;
        self.received = next;
        Ok(())
    }
}

fn validate_record_path(record: &ObjectRecord) -> Result<(), StorageError> {
    let path = Path::new(&record.path);
    if path.as_os_str().is_empty()
        || path.components().any(|part| {
            matches!(
                part,
                Component::ParentDir
                    | Component::CurDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
        || !matches!(record.kind.as_str(), "book" | "wallpaper")
    {
        return Err(StorageError::UnsafePath(path.to_path_buf()));
    }
    Ok(())
}

fn validate_format(record: &ObjectRecord, path: &Path) -> Result<(), StorageError> {
    match record.kind.as_str() {
        "book" => validate_book(path, &record.path)?,
        "wallpaper" => validate_png(path)?,
        _ => return Err(StorageError::InvalidKind),
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<String, io::Error> {
    let mut file = File::open(path)?;
    let mut hasher = Hasher::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        hasher.update(&buffer[..size]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn modified_ms(path: &Path) -> Result<i64, io::Error> {
    Ok(fs::metadata(path)?
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64)
}

fn ensure_root(path: &Path) -> Result<(), StorageError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(StorageError::UnsafePath(path.to_path_buf()));
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<(), io::Error> {
    File::open(
        path.parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?,
    )?
    .sync_all()
}

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("unsafe synchronized path: {0}")]
    UnsafePath(PathBuf),
    #[error("unsupported synchronized object kind")]
    InvalidKind,
    #[error("cannot transfer a deleted source")]
    DeletedSource,
    #[error("record is not a tombstone")]
    NotDeleted,
    #[error("received file size differs: expected {expected}, got {actual}")]
    Size { expected: u64, actual: u64 },
    #[error("received file hash differs")]
    Hash,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Catalog(#[from] crate::catalog::CatalogError),
    #[error(transparent)]
    Trash(#[from] crate::trash::TrashError),
    #[error(transparent)]
    Upload(#[from] crate::upload::UploadError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn winner_pairs_are_deterministic() {
        let record = ObjectRecord {
            kind: "book".into(),
            path: "a.epub".into(),
            size: 1,
            hash: "0".repeat(64),
            version: crate::catalog::VersionStamp {
                wall_time_ms: 1,
                counter: 1,
                origin: "0123456789abcdef".into(),
            },
            deleted: false,
        };
        let pairs =
            PeerStorage::winners(std::slice::from_ref(&record), std::slice::from_ref(&record));
        assert_eq!(pairs.len(), 1);
        assert!(pairs[0].0.is_some() && pairs[0].1.is_some());
    }
}
