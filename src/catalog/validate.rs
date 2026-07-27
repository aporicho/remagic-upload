use super::{CatalogError, ObjectRecord, TrustedPeer};
use std::fs;
use std::io;
use std::path::{Component, Path};

pub(super) fn validate_kind(kind: &str) -> Result<(), CatalogError> {
    if matches!(
        kind,
        "book"
            | "wallpaper"
            | "home_settings"
            | "koreader_data"
            | "koreader_legacy_data"
            | "koreader_sidecar"
            | "magicpaper_data"
            | "magicpaper_config"
            | "magicpaper_legacy_data"
            | "magicpaper_legacy_config"
            | "magicpaper_riddle_data"
            | "magicpaper_riddle_config"
            | "remagic_secret"
            | "magicpaper_oracle_secret"
    ) {
        Ok(())
    } else {
        Err(CatalogError::InvalidKind(kind.to_owned()))
    }
}

pub(super) fn validate_record(record: &ObjectRecord) -> Result<(), CatalogError> {
    validate_kind(&record.kind)?;
    if record.path.is_empty()
        || record.path.len() > 1024
        || Path::new(&record.path).components().any(|component| {
            matches!(
                component,
                Component::ParentDir
                    | Component::CurDir
                    | Component::RootDir
                    | Component::Prefix(_)
            )
        })
        || record.hash.len() != 64
        || !record.hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        || record.mode > 0o777
        || record.version.origin.len() != 16
    {
        return Err(CatalogError::InvalidRecord(record.path.clone()));
    }
    Ok(())
}

pub(super) fn validate_peer(peer: &TrustedPeer) -> Result<(), CatalogError> {
    if peer.id.len() != 16 || peer.public_key.len() != 32 || peer.name.trim().is_empty() {
        Err(CatalogError::InvalidPeer)
    } else {
        Ok(())
    }
}

pub(super) fn create_private_directory(path: &Path) -> Result<(), io::Error> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
