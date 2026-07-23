mod naming;
pub(crate) mod validate;

use crate::server::SharedStatus;
use crate::{catalog::Catalog, trash::Trash};
use naming::{collision_candidate, sanitize_filename};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use thiserror::Error;
use validate::{validate_book, validate_png};

pub const BOOK_MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
pub const WALLPAPER_MAX_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UploadKind {
    Book,
    Wallpaper,
}

impl UploadKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "book" => Some(Self::Book),
            "wallpaper" => Some(Self::Wallpaper),
            _ => None,
        }
    }

    fn max_bytes(self) -> u64 {
        match self {
            Self::Book => BOOK_MAX_BYTES,
            Self::Wallpaper => WALLPAPER_MAX_BYTES,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Book => "book",
            Self::Wallpaper => "wallpaper",
        }
    }
}

pub struct UploadRegistry {
    books_dir: PathBuf,
    wallpapers_dir: PathBuf,
    catalog: Option<Arc<Catalog>>,
    trash: Option<Trash>,
}

impl UploadRegistry {
    #[cfg(test)]
    pub fn new(books_dir: PathBuf, wallpapers_dir: PathBuf) -> Result<Self, UploadError> {
        validate_destination(&books_dir)?;
        validate_destination(&wallpapers_dir)?;
        cleanup_parts(&books_dir)?;
        cleanup_parts(&wallpapers_dir)?;
        Ok(Self {
            books_dir,
            wallpapers_dir,
            catalog: None,
            trash: None,
        })
    }

    pub fn managed(
        books_dir: PathBuf,
        wallpapers_dir: PathBuf,
        catalog: Arc<Catalog>,
        data_home: &Path,
    ) -> Result<Self, UploadError> {
        validate_destination(&books_dir)?;
        validate_destination(&wallpapers_dir)?;
        cleanup_parts(&books_dir)?;
        cleanup_parts(&wallpapers_dir)?;
        Ok(Self {
            books_dir,
            wallpapers_dir,
            catalog: Some(catalog),
            trash: Some(Trash::new(data_home)?),
        })
    }

    pub fn receive(
        &self,
        kind: UploadKind,
        filename: &str,
        length: u64,
        stream: &mut dyn Read,
        stopping: &AtomicBool,
        status: &Arc<SharedStatus>,
    ) -> Result<PathBuf, UploadError> {
        if length == 0 || length > kind.max_bytes() {
            return Err(UploadError::InvalidSize {
                length,
                maximum: kind.max_bytes(),
            });
        }
        let filename = sanitize_filename(filename, kind)?;
        let destination = match kind {
            UploadKind::Book => &self.books_dir,
            UploadKind::Wallpaper => &self.wallpapers_dir,
        };
        let temporary = destination.join(format!(".remagic-upload-{}.part", random_suffix()?));
        let result = receive_to_temp(
            &temporary,
            filename.as_str(),
            length,
            stream,
            stopping,
            status,
        )
        .and_then(|()| {
            match kind {
                UploadKind::Book => validate_book(&temporary, &filename)?,
                UploadKind::Wallpaper => validate_png(&temporary)?,
            }
            if let (Some(catalog), Some(trash)) = (&self.catalog, &self.trash) {
                publish_replacing(&temporary, destination, &filename, kind, catalog, trash)
            } else {
                publish_no_overwrite(&temporary, destination, &filename)
            }
        });
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
}

fn publish_replacing(
    temporary: &Path,
    destination: &Path,
    filename: &str,
    kind: UploadKind,
    catalog: &Catalog,
    trash: &Trash,
) -> Result<PathBuf, UploadError> {
    let final_path = destination.join(filename);
    let previous = trash.move_existing(kind.as_str(), Path::new(filename), &final_path)?;
    if let Err(error) = fs::rename(temporary, &final_path) {
        if let Some(previous) = previous {
            let _ = trash.restore(&previous, &final_path);
        }
        return Err(error.into());
    }
    if let Err(error) = catalog.record_local_file(kind.as_str(), destination, &final_path) {
        let _ = fs::remove_file(&final_path);
        if let Some(previous) = previous {
            let _ = trash.restore(&previous, &final_path);
        }
        return Err(error.into());
    }
    File::open(destination)?.sync_all()?;
    let _ = trash.prune_expired();
    Ok(final_path)
}

fn receive_to_temp(
    path: &Path,
    filename: &str,
    length: u64,
    stream: &mut dyn Read,
    stopping: &AtomicBool,
    status: &Arc<SharedStatus>,
) -> Result<(), UploadError> {
    let mut output = OpenOptions::new().write(true).create_new(true).open(path)?;
    status.begin(filename, length);
    let mut received = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    while received < length {
        if stopping.load(Ordering::Acquire) {
            return Err(UploadError::Cancelled);
        }
        let wanted = (length - received).min(buffer.len() as u64) as usize;
        match stream.read(&mut buffer[..wanted]) {
            Ok(0) => {
                return Err(UploadError::Truncated { received, length });
            }
            Ok(size) => {
                output.write_all(&buffer[..size])?;
                received += size as u64;
                status.progress(received);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(error) => return Err(error.into()),
        }
    }
    output.sync_all()?;
    Ok(())
}

fn publish_no_overwrite(
    temporary: &Path,
    destination: &Path,
    filename: &str,
) -> Result<PathBuf, UploadError> {
    for index in 1..=10_000 {
        let candidate = destination.join(collision_candidate(filename, index));
        match fs::hard_link(temporary, &candidate) {
            Ok(()) => {
                fs::remove_file(temporary)?;
                File::open(destination)?.sync_all()?;
                return Ok(candidate);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(UploadError::CollisionLimit)
}

fn validate_destination(path: &Path) -> Result<(), UploadError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(UploadError::UnsafeDestination(path.to_path_buf()));
    }
    Ok(())
}

fn cleanup_parts(path: &Path) -> Result<(), UploadError> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(".remagic-upload-")
            && entry.file_type()?.is_file()
        {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
}

fn random_suffix() -> Result<String, UploadError> {
    let mut bytes = [0_u8; 12];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[derive(Debug, Error)]
pub enum UploadError {
    #[error("上传大小 {length} 不在允许范围内（最大 {maximum}）")]
    InvalidSize { length: u64, maximum: u64 },
    #[error("文件名无效：{0}")]
    InvalidFilename(String),
    #[error("文件格式与扩展名不匹配：{0}")]
    InvalidFormat(String),
    #[error("上传被取消")]
    Cancelled,
    #[error("上传中断：仅收到 {received}/{length} 字节")]
    Truncated { received: u64, length: u64 },
    #[error("目标目录不安全：{0}")]
    UnsafeDestination(PathBuf),
    #[error("同名文件数量过多")]
    CollisionLimit,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("PNG 解码失败：{0}")]
    Png(String),
    #[error(transparent)]
    Catalog(#[from] crate::catalog::CatalogError),
    #[error(transparent)]
    Trash(#[from] crate::trash::TrashError),
}

#[cfg(test)]
mod tests;
