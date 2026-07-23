use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use thiserror::Error;

const MAX_HEADERS: usize = 16 * 1024;
const MAX_PATH: usize = 2048;

pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub content_length: u64,
    pub body_prefix_len: usize,
    pub body_prefix: Vec<u8>,
}

pub fn read_request(
    stream: &mut TcpStream,
    stopping: &AtomicBool,
) -> Result<HttpRequest, HttpError> {
    let mut bytes = Vec::with_capacity(2048);
    let deadline = Instant::now() + Duration::from_secs(5);
    let header_end = loop {
        if stopping.load(Ordering::Acquire) {
            return Err(HttpError::Stopping);
        }
        if Instant::now() >= deadline {
            return Err(HttpError::HeaderTimeout);
        }
        if bytes.len() >= MAX_HEADERS {
            return Err(HttpError::HeadersTooLarge);
        }
        let mut buffer = [0_u8; 2048];
        let read = match stream.read(&mut buffer) {
            Ok(read) => read,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(error) => return Err(error.into()),
        };
        if read == 0 {
            return Err(HttpError::Disconnected);
        }
        bytes.extend_from_slice(&buffer[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    parse_request(&bytes, header_end)
}

fn parse_request(bytes: &[u8], header_end: usize) -> Result<HttpRequest, HttpError> {
    if header_end > bytes.len()
        || header_end < 4
        || &bytes[header_end - 4..header_end] != b"\r\n\r\n"
    {
        return Err(HttpError::InvalidRequestLine);
    }
    let head = std::str::from_utf8(&bytes[..header_end]).map_err(|_| HttpError::InvalidUtf8)?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next().ok_or(HttpError::InvalidRequestLine)?;
    let mut fields = request_line.split_whitespace();
    let method = fields.next().ok_or(HttpError::InvalidRequestLine)?;
    let target = fields.next().ok_or(HttpError::InvalidRequestLine)?;
    let version = fields.next().ok_or(HttpError::InvalidRequestLine)?;
    if fields.next().is_some()
        || !matches!(method, "GET" | "POST" | "PUT")
        || version != "HTTP/1.1"
        || target.len() > MAX_PATH
    {
        return Err(HttpError::InvalidRequestLine);
    }
    let path = target.split('?').next().unwrap_or(target);
    if !path.starts_with('/') || path.contains("..") {
        return Err(HttpError::InvalidPath);
    }
    let mut headers = BTreeMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or(HttpError::InvalidHeader)?;
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || value
                .bytes()
                .any(|byte| (byte < 0x20 && byte != b'\t') || byte == 0x7f)
            || headers.insert(name, value.to_owned()).is_some()
        {
            return Err(HttpError::InvalidHeader);
        }
    }
    if headers.contains_key("transfer-encoding") {
        return Err(HttpError::ChunkedUnsupported);
    }
    let content_length = match headers.get("content-length") {
        Some(value) => value.parse().map_err(|_| HttpError::InvalidContentLength)?,
        None if matches!(method, "POST" | "PUT") => return Err(HttpError::MissingContentLength),
        None => 0,
    };
    let body_prefix = bytes[header_end..].to_vec();
    if body_prefix.len() as u64 > content_length {
        return Err(HttpError::BodyTooLong);
    }
    Ok(HttpRequest {
        method: method.into(),
        path: path.into(),
        headers,
        content_length,
        body_prefix_len: body_prefix.len(),
        body_prefix,
    })
}

pub fn response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Content",
        429 => "Too Many Requests",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' blob: data:\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

#[derive(Debug, Error)]
pub enum HttpError {
    #[error("请求头超过限制")]
    HeadersTooLarge,
    #[error("连接在请求完成前关闭")]
    Disconnected,
    #[error("服务正在停止")]
    Stopping,
    #[error("请求头读取超时")]
    HeaderTimeout,
    #[error("请求头不是 UTF-8")]
    InvalidUtf8,
    #[error("请求行无效")]
    InvalidRequestLine,
    #[error("请求路径无效")]
    InvalidPath,
    #[error("请求头无效")]
    InvalidHeader,
    #[error("不支持分块传输")]
    ChunkedUnsupported,
    #[error("缺少 Content-Length")]
    MissingContentLength,
    #[error("Content-Length 无效")]
    InvalidContentLength,
    #[error("请求正文超过 Content-Length")]
    BodyTooLong,
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_preserves_prefetched_body_and_requires_exact_length() {
        let request =
            b"PUT /api/upload HTTP/1.1\r\nHost: 10.11.99.1:8787\r\nContent-Length: 3\r\n\r\nabc";
        let end = request
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        let parsed = parse_request(request, end).unwrap();
        assert_eq!(parsed.method, "PUT");
        assert_eq!(parsed.path, "/api/upload");
        assert_eq!(parsed.content_length, 3);
        assert_eq!(parsed.body_prefix, b"abc");
    }

    #[test]
    fn parser_rejects_chunked_missing_length_duplicates_and_controls() {
        for request in [
            b"PUT /api/upload HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n".as_slice(),
            b"POST /api/session HTTP/1.1\r\nHost: device\r\n\r\n",
            b"GET / HTTP/1.1\r\nHost: one\r\nHost: two\r\n\r\n",
            b"GET / HTTP/1.1\r\nX-Bad: value\x01\r\n\r\n",
        ] {
            let end = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap()
                + 4;
            assert!(parse_request(request, end).is_err());
        }
    }
}
