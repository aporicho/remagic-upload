use super::*;
use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT: AtomicU64 = AtomicU64::new(1);

#[test]
fn browser_pair_and_upload_round_trip() {
    let books = temp_dir("books");
    let wallpapers = temp_dir("wallpapers");
    let credentials = Credentials {
        pin: "123456".into(),
        bootstrap: "0123456789abcdef0123456789abcdef".into(),
        bearer: "0123456789abcdef0123456789abcdef0123456789abcdef01234567".into(),
    };
    let server = ServerHandle::start(ServerConfig {
        bind: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0).into(),
        credentials: credentials.clone(),
        registry: UploadRegistry::new(books.clone(), wallpapers.clone()).unwrap(),
        status: Arc::new(SharedStatus::new()),
    })
    .unwrap();
    let address = server.address();

    let session_body = br#"{"pin":"123456"}"#;
    let session = request(
        address,
        &format!(
            "POST /api/session HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nContent-Length: {}\r\n\r\n",
            session_body.len()
        ),
        session_body,
    );
    assert!(session.starts_with("HTTP/1.1 200"));
    let value: serde_json::Value = serde_json::from_slice(response_body(&session)).unwrap();
    assert_eq!(value["token"], credentials.bearer);

    let book = b"%PDF-1.7\nserver round trip\n";
    let upload = request(
        address,
        &format!(
            "PUT /api/upload HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nAuthorization: Bearer {}\r\nX-Upload-Kind: book\r\nX-Filename: %E6%B5%8B%E8%AF%95.pdf\r\nContent-Length: {}\r\n\r\n",
            credentials.bearer,
            book.len()
        ),
        book,
    );
    assert!(upload.starts_with("HTTP/1.1 201"), "{upload}");
    assert_eq!(fs::read(books.join("测试.pdf")).unwrap(), book);

    let rejected = request(
        address,
        &format!("GET / HTTP/1.1\r\nHost: {address}\r\nOrigin: http://attacker.invalid\r\n\r\n"),
        &[],
    );
    assert!(rejected.starts_with("HTTP/1.1 403"));

    let mut slow = TcpStream::connect(address).unwrap();
    slow.write_all(b"GET / HTTP/1.1\r\n").unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let stop_started = Instant::now();
    server.stop();
    assert!(stop_started.elapsed() < Duration::from_secs(1));
    fs::remove_dir_all(books).unwrap();
    fs::remove_dir_all(wallpapers).unwrap();
}

fn request(address: SocketAddr, head: &str, body: &[u8]) -> String {
    let mut stream = TcpStream::connect(address).unwrap();
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

fn response_body(response: &str) -> &[u8] {
    response
        .split_once("\r\n\r\n")
        .expect("HTTP response has a header terminator")
        .1
        .as_bytes()
}

fn temp_dir(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "remagic-upload-server-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&path).unwrap();
    path
}
