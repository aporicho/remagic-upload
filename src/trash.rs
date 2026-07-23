use crate::catalog::now_ms;
use std::fs::{self, File};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};
use thiserror::Error;

const RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Clone, Debug)]
pub struct Trash {
    root: PathBuf,
}

impl Trash {
    pub fn new(data_home: &Path) -> Result<Self, TrashError> {
        let root = data_home.join("trash");
        create_dir(&root)?;
        Ok(Self { root })
    }

    pub fn move_existing(
        &self,
        kind: &str,
        relative: &Path,
        source: &Path,
    ) -> Result<Option<PathBuf>, TrashError> {
        match fs::symlink_metadata(source) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(TrashError::UnsafeSource(source.to_path_buf()))
            }
            Ok(_) => {}
        }
        validate_relative(relative)?;
        let bucket = self.root.join(format!("{}-{}", now_ms(), kind));
        let destination = bucket.join(relative);
        let parent = destination.parent().ok_or(TrashError::InvalidRelative)?;
        create_dir(parent)?;
        fs::rename(source, &destination)?;
        sync_directory(parent)?;
        Ok(Some(destination))
    }

    pub fn restore(&self, trashed: &Path, destination: &Path) -> Result<(), TrashError> {
        let canonical_root = self.root.canonicalize()?;
        let metadata = fs::symlink_metadata(trashed)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(TrashError::UnsafeSource(trashed.to_path_buf()));
        }
        let parent = trashed.parent().ok_or(TrashError::InvalidRelative)?;
        let destination_parent = destination.parent().ok_or(TrashError::InvalidRelative)?;
        let destination_parent_metadata = fs::symlink_metadata(destination_parent)?;
        if destination_parent_metadata.file_type().is_symlink()
            || !destination_parent_metadata.is_dir()
            || !parent.canonicalize()?.starts_with(&canonical_root)
            || destination.exists()
        {
            return Err(TrashError::UnsafeSource(trashed.to_path_buf()));
        }
        fs::rename(trashed, destination)?;
        if let Some(parent) = destination.parent() {
            sync_directory(parent)?;
        }
        Ok(())
    }

    pub fn prune_expired(&self) -> Result<usize, TrashError> {
        let cutoff = SystemTime::now()
            .checked_sub(RETENTION)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut removed = 0;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let metadata = fs::symlink_metadata(entry.path())?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                continue;
            }
            if metadata.modified().unwrap_or(SystemTime::now()) < cutoff {
                fs::remove_dir_all(entry.path())?;
                removed += 1;
            }
        }
        if removed > 0 {
            sync_directory(&self.root)?;
        }
        Ok(removed)
    }
}

fn validate_relative(path: &Path) -> Result<(), TrashError> {
    if path.as_os_str().is_empty()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir
                    | Component::CurDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        Err(TrashError::InvalidRelative)
    } else {
        Ok(())
    }
}

fn create_dir(path: &Path) -> Result<(), io::Error> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), io::Error> {
    File::open(path)?.sync_all()
}

#[derive(Debug, Error)]
pub enum TrashError {
    #[error("trash path must be relative and normalized")]
    InvalidRelative,
    #[error("unsafe trash source: {0}")]
    UnsafeSource(PathBuf),
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_and_restore_never_overwrite_live_content() {
        let root = std::env::temp_dir().join(format!("remagic-trash-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let trash = Trash::new(&root.join("data")).unwrap();
        let live = root.join("book.epub");
        fs::write(&live, b"old").unwrap();
        let moved = trash
            .move_existing("book", Path::new("book.epub"), &live)
            .unwrap()
            .unwrap();
        assert!(!live.exists());
        fs::write(&live, b"new").unwrap();
        assert!(trash.restore(&moved, &live).is_err());
        fs::remove_file(&live).unwrap();
        trash.restore(&moved, &live).unwrap();
        assert_eq!(fs::read(&live).unwrap(), b"old");
        fs::remove_dir_all(root).unwrap();
    }
}
