use super::UploadError;
use crate::catalog::{now_ms, Catalog};
use serde_json::json;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const RECORD_KIND: &str = "xochitl_document";
const SIDECARS: [&str; 4] = ["metadata", "content", "local", "pagedata"];

pub(super) fn is_native_library(path: &Path) -> bool {
    path.ends_with(".local/share/remarkable/xochitl")
}

pub(super) fn publish_book(
    temporary: &Path,
    root: &Path,
    filename: &str,
    catalog: &Catalog,
) -> Result<PathBuf, UploadError> {
    let extension = extension(filename)?;
    let title = title(filename);
    let uuid = unused_uuid(root, &extension)?;
    let document = root.join(format!("{uuid}.{extension}"));
    fs::rename(temporary, &document)?;
    let mut published = vec![document.clone()];
    if let Err(error) = write_sidecars(root, &uuid, &extension, &title, &mut published)
        .and_then(|()| record_all(root, catalog, &published))
    {
        for path in published.iter().rev() {
            let _ = fs::remove_file(path);
        }
        return Err(error);
    }
    File::open(root)?.sync_all()?;
    Ok(document)
}

fn write_sidecars(
    root: &Path,
    uuid: &str,
    extension: &str,
    title: &str,
    published: &mut Vec<PathBuf>,
) -> Result<(), UploadError> {
    let created = now_ms().to_string();
    let metadata = json!({
        "createdTime": created,
        "lastModified": created,
        "lastOpened": "0",
        "lastOpenedPage": 0,
        "new": true,
        "parent": "",
        "pinned": false,
        "source": "",
        "type": "DocumentType",
        "visibleName": title,
    });
    write_json(root, uuid, "metadata", &metadata, published)?;

    let content = json!({
        "coverPageNumber": 0,
        "customZoomCenterX": 0,
        "customZoomCenterY": 936,
        "customZoomOrientation": "portrait",
        "customZoomPageHeight": 1872,
        "customZoomPageWidth": 1404,
        "customZoomScale": 1,
        "documentMetadata": {
            "title": title,
        },
        "extraMetadata": {},
        "fileType": extension,
        "fontName": "",
        "formatVersion": 1,
        "lineHeight": -1,
        "margins": 225,
        "orientation": "portrait",
        "originalPageCount": 1,
        "pageCount": 1,
        "pageTags": [],
        "pages": [],
    });
    write_json(root, uuid, "content", &content, published)?;
    write_json(
        root,
        uuid,
        "local",
        &json!({ "contentFormatVersion": 1 }),
        published,
    )?;
    write_atomic(root, uuid, "pagedata", b"", published)?;
    Ok(())
}

fn write_json(
    root: &Path,
    uuid: &str,
    suffix: &str,
    value: &serde_json::Value,
    published: &mut Vec<PathBuf>,
) -> Result<(), UploadError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    write_atomic(root, uuid, suffix, &bytes, published)
}

fn write_atomic(
    root: &Path,
    uuid: &str,
    suffix: &str,
    bytes: &[u8],
    published: &mut Vec<PathBuf>,
) -> Result<(), UploadError> {
    let final_path = root.join(format!("{uuid}.{suffix}"));
    let temporary = root.join(format!(".remagic-upload-{uuid}.{suffix}.part"));
    let mut output = File::options()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    output.write_all(bytes)?;
    output.sync_all()?;
    drop(output);
    fs::rename(&temporary, &final_path)?;
    published.push(final_path);
    Ok(())
}

fn record_all(root: &Path, catalog: &Catalog, paths: &[PathBuf]) -> Result<(), UploadError> {
    for path in paths {
        catalog.record_local_file(RECORD_KIND, root, path)?;
    }
    Ok(())
}

fn unused_uuid(root: &Path, extension: &str) -> Result<String, UploadError> {
    for _ in 0..100 {
        let uuid = random_uuid()?;
        if !root.join(format!("{uuid}.{extension}")).exists()
            && SIDECARS
                .iter()
                .all(|suffix| !root.join(format!("{uuid}.{suffix}")).exists())
        {
            return Ok(uuid);
        }
    }
    Err(UploadError::CollisionLimit)
}

fn random_uuid() -> Result<String, UploadError> {
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    ))
}

fn extension(filename: &str) -> Result<String, UploadError> {
    Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| UploadError::InvalidFilename(filename.to_owned()))
}

fn title(filename: &str) -> String {
    Path::new(filename)
        .file_stem()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(filename)
        .to_owned()
}
