use super::*;
use std::fs;
use std::io::Cursor;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

static NEXT: AtomicU64 = AtomicU64::new(1);

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "remagic-upload-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    path
}

#[test]
fn publication_never_overwrites_an_existing_book() {
    let directory = temp_dir();
    fs::write(directory.join("论语.epub"), b"old").unwrap();
    let temporary = directory.join(".remagic-upload-test.part");
    fs::write(&temporary, b"new").unwrap();
    let published = publish_no_overwrite(&temporary, &directory, "论语.epub").unwrap();
    assert_eq!(published.file_name().unwrap(), "论语（二）.epub");
    assert_eq!(fs::read(directory.join("论语.epub")).unwrap(), b"old");
    assert_eq!(fs::read(published).unwrap(), b"new");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn registry_removes_only_its_own_abandoned_parts() {
    let books = temp_dir();
    let wallpapers = temp_dir();
    fs::write(books.join(".remagic-upload-old.part"), b"partial").unwrap();
    fs::write(books.join("keep.part"), b"foreign").unwrap();
    UploadRegistry::new(books.clone(), wallpapers.clone()).unwrap();
    assert!(!books.join(".remagic-upload-old.part").exists());
    assert!(books.join("keep.part").exists());
    fs::remove_dir_all(books).unwrap();
    fs::remove_dir_all(wallpapers).unwrap();
}

#[test]
fn registry_streams_valid_content_and_preserves_the_original_name() {
    let books = temp_dir();
    let wallpapers = temp_dir();
    let registry = UploadRegistry::new(books.clone(), wallpapers.clone()).unwrap();
    let payload = b"%PDF-1.7\nminimal fixture\n";
    let mut reader = Cursor::new(payload);
    let stopping = AtomicBool::new(false);
    let status = Arc::new(SharedStatus::new());
    let published = registry
        .receive(
            UploadKind::Book,
            "%E6%B5%8B%E8%AF%95.pdf",
            payload.len() as u64,
            &mut reader,
            &stopping,
            &status,
        )
        .unwrap();
    assert_eq!(published.file_name().unwrap(), "测试.pdf");
    assert_eq!(fs::read(published).unwrap(), payload);
    assert!(!books.read_dir().unwrap().any(|entry| entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .ends_with(".part")));
    fs::remove_dir_all(books).unwrap();
    fs::remove_dir_all(wallpapers).unwrap();
}

#[test]
fn interrupted_stream_removes_the_temporary_file() {
    let books = temp_dir();
    let wallpapers = temp_dir();
    let registry = UploadRegistry::new(books.clone(), wallpapers.clone()).unwrap();
    let mut reader = Cursor::new(b"%PDF-".as_slice());
    let status = Arc::new(SharedStatus::new());
    let error = registry
        .receive(
            UploadKind::Book,
            "broken.pdf",
            20,
            &mut reader,
            &AtomicBool::new(false),
            &status,
        )
        .unwrap_err();
    assert!(matches!(error, UploadError::Truncated { .. }));
    assert_eq!(fs::read_dir(&books).unwrap().count(), 0);
    fs::remove_dir_all(books).unwrap();
    fs::remove_dir_all(wallpapers).unwrap();
}

#[test]
fn epub_and_cbz_require_real_central_directory_entries() {
    let directory = temp_dir();
    let epub = directory.join("book.epub");
    fs::write(
        &epub,
        minimal_zip(
            b"mimetype",
            b"application/epub+zip",
            b"META-INF/container.xml",
        ),
    )
    .unwrap();
    validate_book(&epub, "book.epub").unwrap();

    let cbz = directory.join("comic.cbz");
    fs::write(&cbz, minimal_zip(b"note.txt", b"x", b"pages/001.JPG")).unwrap();
    validate_book(&cbz, "comic.cbz").unwrap();
    fs::write(&cbz, minimal_zip(b"note.txt", b"x", b"pages/readme.txt")).unwrap();
    assert!(validate_book(&cbz, "comic.cbz").is_err());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn wallpaper_requires_a_complete_bounded_png() {
    let directory = temp_dir();
    let wallpaper = directory.join("wall.png");
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 64, 64);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&vec![255; 64 * 64]).unwrap();
    }
    fs::write(&wallpaper, &bytes).unwrap();
    validate_png(&wallpaper).unwrap();
    fs::write(&wallpaper, &bytes[..bytes.len() / 2]).unwrap();
    assert!(validate_png(&wallpaper).is_err());
    fs::remove_dir_all(directory).unwrap();
}

fn minimal_zip(first_name: &[u8], first_data: &[u8], central_name: &[u8]) -> Vec<u8> {
    let mut archive = vec![0_u8; 30];
    archive[..4].copy_from_slice(b"PK\x03\x04");
    archive[18..22].copy_from_slice(&(first_data.len() as u32).to_le_bytes());
    archive[22..26].copy_from_slice(&(first_data.len() as u32).to_le_bytes());
    archive[26..28].copy_from_slice(&(first_name.len() as u16).to_le_bytes());
    archive.extend_from_slice(first_name);
    archive.extend_from_slice(first_data);

    let central_offset = archive.len() as u32;
    let mut central = vec![0_u8; 46];
    central[..4].copy_from_slice(b"PK\x01\x02");
    central[28..30].copy_from_slice(&(central_name.len() as u16).to_le_bytes());
    archive.extend_from_slice(&central);
    archive.extend_from_slice(central_name);
    let central_size = archive.len() as u32 - central_offset;

    let mut end = vec![0_u8; 22];
    end[..4].copy_from_slice(b"PK\x05\x06");
    end[8..10].copy_from_slice(&1_u16.to_le_bytes());
    end[10..12].copy_from_slice(&1_u16.to_le_bytes());
    end[12..16].copy_from_slice(&central_size.to_le_bytes());
    end[16..20].copy_from_slice(&central_offset.to_le_bytes());
    archive.write_all(&end).unwrap();
    archive
}
