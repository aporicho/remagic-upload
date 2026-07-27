use super::CatalogError;
use blake3::Hasher;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

pub(super) fn collect_files(
    root: &Path,
    directory: &Path,
    output: &mut Vec<(String, PathBuf, fs::Metadata)>,
) -> Result<(), CatalogError> {
    let metadata = fs::symlink_metadata(directory)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(CatalogError::UnsafeFile(directory.to_path_buf()));
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(".remagic-") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            collect_files(root, &path, output)?;
        } else if metadata.is_file() {
            output.push((normalized_relative(root, &path)?, path, metadata));
        }
    }
    Ok(())
}

pub(super) fn normalized_relative(root: &Path, path: &Path) -> Result<String, CatalogError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| CatalogError::UnsafeFile(path.to_path_buf()))?;
    if relative.as_os_str().is_empty()
        || relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir
                    | Component::CurDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
    {
        return Err(CatalogError::UnsafeFile(path.to_path_buf()));
    }
    relative
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| CatalogError::UnsafeFile(path.to_path_buf()))
}

#[cfg(test)]
pub(super) fn supported_object_path(kind: &str, path: &str) -> bool {
    let extension = Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match kind {
        "book" => matches!(
            extension.as_str(),
            "pdf" | "djvu" | "djv" | "mobi" | "azw3" | "fb2" | "epub" | "cbz" | "cbr" | "txt"
        ),
        "wallpaper" => extension == "png",
        _ => false,
    }
}

pub(super) fn hash_file(path: &Path) -> Result<String, io::Error> {
    let mut input = File::open(path)?;
    let mut hasher = Hasher::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub(super) fn modified_ms(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

pub(super) fn file_mode(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        0o644
    }
}
