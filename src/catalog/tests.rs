use super::*;
use rand::RngCore;

fn fixture() -> PathBuf {
    let mut random = [0_u8; 8];
    rand::thread_rng().fill_bytes(&mut random);
    let root = std::env::temp_dir().join(format!(
        "remagic-transfer-catalog-{}-{}",
        std::process::id(),
        hex::encode(random)
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn scan_versions_changes_and_creates_deletion_tombstones() {
    let root = fixture();
    let data = root.join("data");
    let books = root.join("books");
    fs::create_dir_all(&books).unwrap();
    fs::write(books.join("论语.epub"), b"first").unwrap();
    let catalog = Catalog::open(&data, "Paper Pro").unwrap();
    let first = catalog.scan("book", &books).unwrap();
    assert_eq!(first.len(), 1);
    assert!(!first[0].deleted);
    let unchanged = catalog.scan("book", &books).unwrap();
    assert_eq!(unchanged[0].version, first[0].version);
    fs::remove_file(books.join("论语.epub")).unwrap();
    let deleted = catalog.scan("book", &books).unwrap();
    assert!(deleted[0].deleted);
    assert!(deleted[0].version > first[0].version);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn identity_and_trusted_peer_survive_reopen() {
    let root = fixture();
    let data = root.join("data");
    let id = {
        let catalog = Catalog::open(&data, "Paper Pro").unwrap();
        let id = catalog.identity().id.clone();
        catalog
            .trust_peer(&TrustedPeer {
                id: "0123456789abcdef".into(),
                name: "Paper Pro Move".into(),
                public_key: vec![7; 32],
            })
            .unwrap();
        id
    };
    let catalog = Catalog::open(&data, "Paper Pro").unwrap();
    assert_eq!(catalog.identity().id, id);
    assert!(catalog.trusted_peer("0123456789abcdef").unwrap().is_some());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn scan_excludes_koreader_sidecars_and_unsupported_files() {
    let root = fixture();
    let data = root.join("data");
    let books = root.join("books");
    let sidecar = books.join("论语.sdr");
    fs::create_dir_all(&sidecar).unwrap();
    fs::write(books.join("论语.epub"), b"book").unwrap();
    fs::write(sidecar.join("metadata.epub.lua"), b"return {}").unwrap();
    fs::write(books.join("cover.png"), b"not a book").unwrap();
    let catalog = Catalog::open(&data, "Paper Pro").unwrap();
    let records = catalog.scan("book", &books).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].path, "论语.epub");
    fs::remove_dir_all(root).unwrap();
}
