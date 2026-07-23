use super::UploadError;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

pub(super) fn validate_book(path: &Path, filename: &str) -> Result<(), UploadError> {
    let extension = Path::new(filename)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut file = File::open(path)?;
    let mut head = vec![0_u8; 64 * 1024];
    let read = file.read(&mut head)?;
    head.truncate(read);
    let valid = match extension.as_str() {
        "pdf" => head.starts_with(b"%PDF-"),
        "djvu" | "djv" => {
            head.starts_with(b"AT&TFORM")
                && head
                    .get(12..16)
                    .is_some_and(|value| value == b"DJVU" || value == b"DJVM")
        }
        "mobi" | "azw3" => head.get(60..68).is_some_and(|value| value == b"BOOKMOBI"),
        "fb2" => contains_ascii_case_insensitive(&head, b"<fictionbook"),
        "epub" => {
            valid_epub_header(&head)
                && zip_has_entry(&mut file, |name| {
                    name.eq_ignore_ascii_case(b"META-INF/container.xml")
                })?
        }
        "cbz" => {
            head.starts_with(b"PK\x03\x04")
                && zip_has_entry(&mut file, |name| {
                    [b".jpg".as_slice(), b".jpeg", b".png", b".webp", b".gif"]
                        .iter()
                        .any(|extension| ends_ascii_case_insensitive(name, extension))
                })?
        }
        "cbr" => head.starts_with(b"Rar!\x1a\x07\x00") || head.starts_with(b"Rar!\x1a\x07\x01\x00"),
        "txt" => validate_utf8(&mut file)?,
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err(UploadError::InvalidFormat(filename.to_owned()))
    }
}

pub(super) fn validate_png(path: &Path) -> Result<(), UploadError> {
    let file = File::open(path)?;
    let decoder = png::Decoder::new_with_limits(
        BufReader::new(file),
        png::Limits {
            bytes: 64 * 1024 * 1024,
        },
    );
    let mut reader = decoder
        .read_info()
        .map_err(|error| UploadError::Png(error.to_string()))?;
    let info = reader.info();
    if !(64..=8192).contains(&info.width) || !(64..=8192).contains(&info.height) {
        return Err(UploadError::InvalidFormat(format!(
            "壁纸尺寸必须在 64–8192 像素之间，实际为 {}×{}",
            info.width, info.height
        )));
    }
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| UploadError::Png("解码后图像过大".into()))?;
    if size > 64 * 1024 * 1024 {
        return Err(UploadError::Png("解码后图像超过 64 MiB".into()));
    }
    let mut decoded = vec![0_u8; size];
    reader
        .next_frame(&mut decoded)
        .map_err(|error| UploadError::Png(error.to_string()))?;
    Ok(())
}

fn valid_epub_header(head: &[u8]) -> bool {
    if !head.starts_with(b"PK\x03\x04") || head.len() < 30 {
        return false;
    }
    let compression = u16::from_le_bytes(head[8..10].try_into().unwrap());
    let name_len = u16::from_le_bytes(head[26..28].try_into().unwrap()) as usize;
    let extra_len = u16::from_le_bytes(head[28..30].try_into().unwrap()) as usize;
    let name_end = 30 + name_len;
    let data_start = name_end + extra_len;
    compression == 0
        && head.get(30..name_end) == Some(b"mimetype")
        && head
            .get(data_start..data_start + 20)
            .is_some_and(|value| value == b"application/epub+zip")
}

fn zip_has_entry(file: &mut File, matches: impl Fn(&[u8]) -> bool) -> Result<bool, UploadError> {
    let length = file.metadata()?.len();
    let tail_len = length.min(65_557) as usize;
    file.seek(SeekFrom::End(-(tail_len as i64)))?;
    let mut tail = vec![0_u8; tail_len];
    file.read_exact(&mut tail)?;
    let Some(eocd) = tail.windows(4).rposition(|window| window == b"PK\x05\x06") else {
        return Ok(false);
    };
    if eocd + 22 > tail.len() {
        return Ok(false);
    }
    let entry_count = u16::from_le_bytes(tail[eocd + 10..eocd + 12].try_into().unwrap());
    let central_size = u32::from_le_bytes(tail[eocd + 12..eocd + 16].try_into().unwrap()) as u64;
    let central_offset = u32::from_le_bytes(tail[eocd + 16..eocd + 20].try_into().unwrap()) as u64;
    if entry_count == u16::MAX
        || central_size > 64 * 1024 * 1024
        || central_offset.saturating_add(central_size) > length
    {
        return Ok(false);
    }
    file.seek(SeekFrom::Start(central_offset))?;
    let mut consumed = 0_u64;
    for _ in 0..entry_count {
        if consumed.saturating_add(46) > central_size {
            return Ok(false);
        }
        let mut header = [0_u8; 46];
        file.read_exact(&mut header)?;
        if !header.starts_with(b"PK\x01\x02") {
            return Ok(false);
        }
        let name_length = u16::from_le_bytes(header[28..30].try_into().unwrap()) as usize;
        let extra_length = u16::from_le_bytes(header[30..32].try_into().unwrap()) as u64;
        let comment_length = u16::from_le_bytes(header[32..34].try_into().unwrap()) as u64;
        let entry_size = 46_u64
            .saturating_add(name_length as u64)
            .saturating_add(extra_length)
            .saturating_add(comment_length);
        if consumed.saturating_add(entry_size) > central_size {
            return Ok(false);
        }
        let mut name = vec![0_u8; name_length];
        file.read_exact(&mut name)?;
        if matches(&name) {
            return Ok(true);
        }
        file.seek(SeekFrom::Current((extra_length + comment_length) as i64))?;
        consumed += entry_size;
    }
    Ok(false)
}

fn validate_utf8(file: &mut File) -> Result<bool, UploadError> {
    file.seek(SeekFrom::Start(0))?;
    let mut carry = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(std::str::from_utf8(&carry).is_ok());
        }
        carry.extend_from_slice(&buffer[..read]);
        match std::str::from_utf8(&carry) {
            Ok(_) => carry.clear(),
            Err(error) if error.error_len().is_none() && carry.len() - error.valid_up_to() <= 3 => {
                carry.drain(..error.valid_up_to());
            }
            Err(_) => return Ok(false),
        }
    }
}

fn contains_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| {
        window
            .iter()
            .zip(needle)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
    })
}

fn ends_ascii_case_insensitive(value: &[u8], suffix: &[u8]) -> bool {
    value.len() >= suffix.len()
        && value[value.len() - suffix.len()..]
            .iter()
            .zip(suffix)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epub_requires_the_uncompressed_mimetype_first_entry() {
        let mut header = vec![0_u8; 30];
        header[..4].copy_from_slice(b"PK\x03\x04");
        header[26..28].copy_from_slice(&8_u16.to_le_bytes());
        header.extend_from_slice(b"mimetype");
        header.extend_from_slice(b"application/epub+zip");
        assert!(valid_epub_header(&header));
        header[8] = 8;
        assert!(!valid_epub_header(&header));
    }
}
