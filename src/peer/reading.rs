use remagic_app_sdk::SyncClient;
use remagic_core::AppId;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

const MAX_BYTES: u64 = 16 * 1024 * 1024;
const READING_FIELDS: [&str; 4] = [
    "last_xpointer",
    "last_page",
    "percent_finished",
    "bookmarks",
];
const FONT_SIZE_FIELDS: [&str; 2] = ["copt_font_size", "kopt_font_size"];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadingScope {
    pub reading: bool,
    pub font_size: bool,
}

impl ReadingScope {
    pub fn any(self) -> bool {
        self.reading || self.font_size
    }
}

#[derive(Clone)]
pub struct ReadingProvider {
    socket: PathBuf,
    requester: AppId,
    exchange_root: PathBuf,
}

impl ReadingProvider {
    pub fn new(socket: PathBuf, requester: AppId, data_home: &Path) -> Self {
        Self {
            socket,
            requester,
            exchange_root: SyncClient::exchange_root(data_home),
        }
    }

    pub fn export(&self, scope: ReadingScope) -> Result<Vec<u8>, ReadingError> {
        fs::create_dir_all(&self.exchange_root)?;
        let path = self.exchange_root.join("koreader-export.json");
        let _ = fs::remove_file(&path);
        let client = self.client()?;
        client.prepare()?;
        if let Err(error) = client.export(&path) {
            let _ = client.finish();
            return Err(error.into());
        }
        let result = read_bounded(&path);
        let _ = client.finish();
        result.and_then(|bytes| scoped_payload(&bytes, scope))
    }

    pub fn import(&self, bytes: &[u8], scope: ReadingScope) -> Result<(), ReadingError> {
        if bytes.len() as u64 > MAX_BYTES {
            return Err(ReadingError::TooLarge(bytes.len() as u64));
        }
        let bytes = scoped_payload(bytes, scope)?;
        fs::create_dir_all(&self.exchange_root)?;
        let path = self.exchange_root.join("koreader-import.json");
        let temporary = self.exchange_root.join(".koreader-import.tmp");
        fs::write(&temporary, bytes)?;
        fs::rename(&temporary, &path)?;
        let client = self.client()?;
        client.prepare()?;
        if let Err(error) = client.import(&path) {
            let _ = client.finish();
            return Err(error.into());
        }
        client.finish()?;
        Ok(())
    }

    fn client(&self) -> Result<SyncClient, ReadingError> {
        Ok(SyncClient::new(
            self.socket.clone(),
            self.requester.clone(),
            AppId::new("koreader")?,
        ))
    }
}

pub fn merge(local: &[u8], remote: &[u8]) -> Result<Vec<u8>, ReadingError> {
    let local = validate_payload(local)?;
    let remote = validate_payload(remote)?;
    let mut books = BTreeMap::<String, Value>::new();
    for record in local.into_iter().chain(remote) {
        let path = record
            .get("path")
            .and_then(Value::as_str)
            .ok_or(ReadingError::Invalid)?
            .to_owned();
        let updated = record
            .get("updated_at")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if let Some(current) = books.get_mut(&path) {
            let current_updated = current
                .get("updated_at")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let newest = if updated > current_updated {
                record.clone()
            } else {
                current.clone()
            };
            let has_bookmarks =
                current.get("bookmarks").is_some() || record.get("bookmarks").is_some();
            let bookmarks = has_bookmarks.then(|| {
                merge_bookmarks(
                    current.get("bookmarks").and_then(Value::as_array),
                    record.get("bookmarks").and_then(Value::as_array),
                )
            });
            let mut merged = newest;
            if let (Some(object), Some(bookmarks)) = (merged.as_object_mut(), bookmarks) {
                object.insert("bookmarks".into(), Value::Array(bookmarks));
            }
            *current = merged;
        } else {
            books.insert(path, record);
        }
    }
    Ok(serde_json::to_vec(&serde_json::json!({
        "schema": 1,
        "books": books.into_values().collect::<Vec<_>>()
    }))?)
}

fn merge_bookmarks(local: Option<&Vec<Value>>, remote: Option<&Vec<Value>>) -> Vec<Value> {
    let mut by_key = BTreeMap::new();
    for bookmark in local
        .into_iter()
        .flatten()
        .chain(remote.into_iter().flatten())
    {
        let key = serde_json::to_string(bookmark).unwrap_or_default();
        by_key.entry(key).or_insert_with(|| bookmark.clone());
    }
    by_key.into_values().collect()
}

fn validate_payload(bytes: &[u8]) -> Result<Vec<Value>, ReadingError> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(ReadingError::TooLarge(bytes.len() as u64));
    }
    let value: Value = serde_json::from_slice(bytes)?;
    if value.get("schema").and_then(Value::as_u64) != Some(1) {
        return Err(ReadingError::Invalid);
    }
    let books = match value.get("books") {
        Some(Value::Array(books)) => books.clone(),
        // KOReader's LuaJSON historically encoded an empty Lua table as {}.
        // Accept only that empty legacy shape and immediately normalize it;
        // non-empty objects remain invalid rather than becoming ambiguous.
        Some(Value::Object(books)) if books.is_empty() => Vec::new(),
        _ => return Err(ReadingError::Invalid),
    };
    if books.len() > 100_000 {
        return Err(ReadingError::Invalid);
    }
    Ok(books)
}

fn scoped_payload(bytes: &[u8], scope: ReadingScope) -> Result<Vec<u8>, ReadingError> {
    let mut books = validate_payload(bytes)?;
    for record in &mut books {
        if let Some(object) = record.as_object_mut() {
            if !scope.reading {
                for field in READING_FIELDS {
                    object.remove(field);
                }
            }
            if !scope.font_size {
                for field in FONT_SIZE_FIELDS {
                    object.remove(field);
                }
            }
        }
    }
    Ok(serde_json::to_vec(&serde_json::json!({
        "schema": 1,
        "books": books
    }))?)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, ReadingError> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_BYTES {
        return Err(ReadingError::TooLarge(metadata.len()));
    }
    let bytes = fs::read(path)?;
    validate_payload(&bytes)?;
    Ok(bytes)
}

#[derive(Debug, Error)]
pub enum ReadingError {
    #[error("KOReader reading state is invalid")]
    Invalid,
    #[error("KOReader reading state is too large: {0} bytes")]
    TooLarge(u64),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Sync(#[from] remagic_app_sdk::SyncError),
    #[error(transparent)]
    AppId(#[from] remagic_core::manifest::ManifestError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_uses_the_newest_reading_record() {
        let old = br#"{"schema":1,"books":[{"path":"/home/root/books/a.epub","updated_at":1,"last_page":3}]}"#;
        let new = br#"{"schema":1,"books":[{"path":"/home/root/books/a.epub","updated_at":2,"last_page":7}]}"#;
        let merged: Value = serde_json::from_slice(&merge(old, new).unwrap()).unwrap();
        assert_eq!(merged["books"][0]["last_page"], 7);
    }

    #[test]
    fn merge_keeps_bookmarks_from_both_devices() {
        let left = br#"{"schema":1,"books":[{"path":"/home/root/books/a.epub","updated_at":1,"bookmarks":[{"page":1}]}]}"#;
        let right = br#"{"schema":1,"books":[{"path":"/home/root/books/a.epub","updated_at":2,"bookmarks":[{"page":2}]}]}"#;
        let merged: Value = serde_json::from_slice(&merge(left, right).unwrap()).unwrap();
        assert_eq!(merged["books"][0]["bookmarks"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn merge_does_not_create_bookmarks_for_font_size_only_records() {
        let left = br#"{"schema":1,"books":[{"path":"/home/root/books/a.epub","updated_at":1,"copt_font_size":22}]}"#;
        let right = br#"{"schema":1,"books":[{"path":"/home/root/books/a.epub","updated_at":2,"copt_font_size":26}]}"#;
        let merged: Value = serde_json::from_slice(&merge(left, right).unwrap()).unwrap();
        let book = merged["books"][0].as_object().unwrap();
        assert_eq!(book.get("copt_font_size"), Some(&serde_json::json!(26)));
        assert!(!book.contains_key("bookmarks"));
    }

    #[test]
    fn scope_can_keep_reading_without_font_size() {
        let payload = br#"{"schema":1,"books":[{"path":"/home/root/books/a.epub","updated_at":2,"last_page":7,"bookmarks":[{"page":1}],"copt_font_size":26,"kopt_font_size":1}]}"#;
        let scoped: Value = serde_json::from_slice(
            &scoped_payload(
                payload,
                ReadingScope {
                    reading: true,
                    font_size: false,
                },
            )
            .unwrap(),
        )
        .unwrap();
        let book = scoped["books"][0].as_object().unwrap();
        assert!(book.contains_key("last_page"));
        assert!(book.contains_key("bookmarks"));
        assert!(!book.contains_key("copt_font_size"));
        assert!(!book.contains_key("kopt_font_size"));
    }

    #[test]
    fn scope_can_keep_font_size_without_reading() {
        let payload = br#"{"schema":1,"books":[{"path":"/home/root/books/a.epub","updated_at":2,"last_page":7,"bookmarks":[{"page":1}],"copt_font_size":26,"kopt_font_size":1}]}"#;
        let scoped: Value = serde_json::from_slice(
            &scoped_payload(
                payload,
                ReadingScope {
                    reading: false,
                    font_size: true,
                },
            )
            .unwrap(),
        )
        .unwrap();
        let book = scoped["books"][0].as_object().unwrap();
        assert!(!book.contains_key("last_page"));
        assert!(!book.contains_key("bookmarks"));
        assert_eq!(book.get("copt_font_size"), Some(&serde_json::json!(26)));
        assert_eq!(book.get("kopt_font_size"), Some(&serde_json::json!(1)));
    }

    #[test]
    fn legacy_empty_object_is_normalized_to_an_empty_array() {
        let legacy = br#"{"schema":1,"books":{}}"#;
        let current = br#"{"schema":1,"books":[]}"#;
        let merged: Value = serde_json::from_slice(&merge(legacy, current).unwrap()).unwrap();
        assert_eq!(merged["books"], serde_json::json!([]));
    }
}
