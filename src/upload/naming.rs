use super::{UploadError, UploadKind};
use std::path::Path;

const MAX_FILENAME_BYTES: usize = 160;

pub(super) fn sanitize_filename(raw: &str, kind: UploadKind) -> Result<String, UploadError> {
    let decoded = percent_decode(raw)?;
    let trimmed = decoded.trim();
    if trimmed.is_empty()
        || trimmed == "."
        || trimmed == ".."
        || trimmed.contains(['/', '\\', '\0'])
        || trimmed.chars().any(char::is_control)
    {
        return Err(UploadError::InvalidFilename(raw.to_owned()));
    }
    let extension = Path::new(trimmed)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if kind == UploadKind::Wallpaper && extension != "png" {
        return Err(UploadError::InvalidFilename(
            "壁纸必须由浏览器转换为 PNG".into(),
        ));
    }
    if trimmed.len() > MAX_FILENAME_BYTES {
        return Err(UploadError::InvalidFilename(format!(
            "文件名超过 {MAX_FILENAME_BYTES} 字节"
        )));
    }
    Ok(trimmed.to_owned())
}

pub(super) fn collision_candidate(filename: &str, index: usize) -> String {
    if index == 1 {
        return filename.to_owned();
    }
    let path = Path::new(filename);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(filename);
    let extension = path.extension().and_then(|value| value.to_str());
    let suffix = chinese_number(index).unwrap_or_else(|| index.to_string());
    match extension {
        Some(extension) => format!("{stem}（{suffix}）.{extension}"),
        None => format!("{stem}（{suffix}）"),
    }
}

fn chinese_number(number: usize) -> Option<String> {
    const DIGITS: [&str; 10] = ["零", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
    match number {
        0..=9 => Some(DIGITS[number].into()),
        10 => Some("十".into()),
        11..=19 => Some(format!("十{}", DIGITS[number % 10])),
        20..=99 if number.is_multiple_of(10) => Some(format!("{}十", DIGITS[number / 10])),
        20..=99 => Some(format!("{}十{}", DIGITS[number / 10], DIGITS[number % 10])),
        _ => None,
    }
}

fn percent_decode(value: &str) -> Result<String, UploadError> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(UploadError::InvalidFilename(value.to_owned()));
            }
            let high = hex(bytes[index + 1])?;
            let low = hex(bytes[index + 2])?;
            decoded.push(high << 4 | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| UploadError::InvalidFilename(value.to_owned()))
}

fn hex(byte: u8) -> Result<u8, UploadError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(UploadError::InvalidFilename("错误的 URL 编码".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_names_are_decoded_and_collisions_use_chinese_suffixes() {
        assert_eq!(
            sanitize_filename("%E8%AE%BA%E8%AF%AD.epub", UploadKind::Book).unwrap(),
            "论语.epub"
        );
        assert_eq!(collision_candidate("论语.epub", 1), "论语.epub");
        assert_eq!(collision_candidate("论语.epub", 2), "论语（二）.epub");
        assert_eq!(collision_candidate("论语.epub", 12), "论语（十二）.epub");
    }

    #[test]
    fn traversal_and_non_png_wallpapers_are_rejected() {
        assert!(sanitize_filename("../x.epub", UploadKind::Book).is_err());
        assert!(sanitize_filename("wall.jpg", UploadKind::Wallpaper).is_err());
    }
}
