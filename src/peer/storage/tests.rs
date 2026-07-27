use super::*;
use rand::RngCore;

fn fixture() -> PathBuf {
    let mut random = [0_u8; 8];
    rand::thread_rng().fill_bytes(&mut random);
    let root = std::env::temp_dir().join(format!(
        "remagic-peer-storage-{}-{}",
        std::process::id(),
        hex::encode(random)
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

fn storage(root: &Path) -> PeerStorage {
    let books = root.join("books");
    let wallpapers = root.join(".local/share/remagic/wallpapers");
    fs::create_dir_all(&books).unwrap();
    fs::create_dir_all(&wallpapers).unwrap();
    let catalog = Arc::new(Catalog::open(&root.join(".local/share/upload"), "test").unwrap());
    PeerStorage::new(
        books,
        wallpapers,
        catalog,
        &root.join(".local/share/upload"),
    )
    .unwrap()
}

#[test]
fn winner_pairs_are_deterministic() {
    let record = ObjectRecord {
        kind: "book".into(),
        path: "a.epub".into(),
        size: 1,
        hash: "0".repeat(64),
        mode: 0o644,
        version: crate::catalog::VersionStamp {
            wall_time_ms: 1,
            counter: 1,
            origin: "0123456789abcdef".into(),
        },
        deleted: false,
    };
    let pairs = PeerStorage::winners(std::slice::from_ref(&record), std::slice::from_ref(&record));
    assert_eq!(pairs.len(), 1);
    assert!(pairs[0].0.is_some() && pairs[0].1.is_some());
}

#[test]
fn scan_includes_selected_app_data_without_secret_leakage() {
    let root = fixture();
    fs::create_dir_all(root.join("books/a.sdr")).unwrap();
    fs::write(root.join("books/a.epub"), b"book").unwrap();
    fs::write(root.join("books/a.sdr/metadata.epub.lua"), b"return {}").unwrap();
    fs::create_dir_all(root.join(".config/magicpaper")).unwrap();
    fs::write(root.join(".config/magicpaper/preferences.json"), b"{}").unwrap();
    fs::write(root.join(".config/magicpaper/oracle.env"), b"SECRET=1").unwrap();
    fs::create_dir_all(root.join(".config/remagic/secrets/providers")).unwrap();
    fs::write(
        root.join(".config/remagic/secrets/providers/openai.env"),
        b"OPENAI_API_KEY=1",
    )
    .unwrap();

    let storage = storage(&root);
    let records = storage.scan(&SyncSelection::default()).unwrap();
    let keys = records
        .iter()
        .map(|record| (record.kind.as_str(), record.path.as_str()))
        .collect::<Vec<_>>();

    assert!(keys.contains(&("book", "a.epub")));
    assert!(keys.contains(&("koreader_sidecar", "a.sdr/metadata.epub.lua")));
    assert!(keys.contains(&("magicpaper_config", "preferences.json")));
    assert!(!keys.contains(&("magicpaper_config", "oracle.env")));
    assert!(keys.contains(&("magicpaper_oracle_secret", "oracle.env")));
    assert!(keys.contains(&("remagic_secret", "openai.env")));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn received_secret_is_written_private() {
    let root = fixture();
    let storage = storage(&root);
    let path = root.join("source.env");
    fs::write(&path, b"KEY=value").unwrap();
    let record = ObjectRecord {
        kind: "remagic_secret".into(),
        path: "openai.env".into(),
        size: 9,
        hash: hash_file(&path).unwrap(),
        mode: 0o644,
        version: crate::catalog::VersionStamp {
            wall_time_ms: 1,
            counter: 1,
            origin: "0123456789abcdef".into(),
        },
        deleted: false,
    };
    let mut incoming = storage.begin_receive(&record).unwrap();
    incoming.write_chunk(b"KEY=value").unwrap();
    storage.commit(incoming).unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(root.join(".config/remagic/secrets/providers/openai.env"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
    fs::remove_dir_all(root).unwrap();
}
