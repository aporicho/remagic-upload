mod http;
mod status;
#[cfg(test)]
mod tests;

use crate::auth::Credentials;
use crate::peer::{PeerRuntime, MAGIC as PEER_MAGIC};
use crate::upload::{UploadKind, UploadRegistry};
use http::{read_request, response, HttpRequest};
use std::collections::HashMap;
use std::io::{self, Cursor, Read};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};
use thiserror::Error;

const INDEX: &str = include_str!("../../web/index.html");
const SCRIPT: &str = include_str!("../../web/app.js");
const STYLE: &str = include_str!("../../web/style.css");
const MAX_CONNECTIONS: usize = 8;

pub use status::{SharedStatus, StatusSnapshot};

pub struct ServerConfig {
    pub bind: SocketAddr,
    pub credentials: Credentials,
    pub registry: UploadRegistry,
    pub status: Arc<SharedStatus>,
    pub peer: Option<Arc<PeerRuntime>>,
}

pub struct ServerHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    #[cfg(test)]
    address: SocketAddr,
}

impl ServerHandle {
    pub fn start(config: ServerConfig) -> Result<Self, ServerError> {
        let listener = TcpListener::bind(config.bind)?;
        #[cfg(test)]
        let address = listener.local_addr()?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let config = Arc::new(RuntimeConfig {
            credentials: config.credentials,
            registry: config.registry,
            status: config.status,
            peer: config.peer,
            active_upload: AtomicBool::new(false),
            active_connections: AtomicUsize::new(0),
            attempts: Mutex::new(HashMap::new()),
        });
        let thread = thread::Builder::new()
            .name("upload-http".into())
            .spawn(move || serve(listener, worker_stop, config))?;
        Ok(Self {
            stop,
            thread: Some(thread),
            #[cfg(test)]
            address,
        })
    }

    #[cfg(test)]
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct RuntimeConfig {
    credentials: Credentials,
    registry: UploadRegistry,
    status: Arc<SharedStatus>,
    peer: Option<Arc<PeerRuntime>>,
    active_upload: AtomicBool,
    active_connections: AtomicUsize,
    attempts: Mutex<HashMap<IpAddr, AttemptWindow>>,
}

struct AttemptWindow {
    since: Instant,
    failures: u8,
}

fn serve(listener: TcpListener, stop: Arc<AtomicBool>, config: Arc<RuntimeConfig>) {
    let mut workers: Vec<JoinHandle<()>> = Vec::new();
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, remote)) => {
                if config
                    .active_connections
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |connections| {
                        (connections < MAX_CONNECTIONS).then_some(connections + 1)
                    })
                    .is_err()
                {
                    reject_busy(stream);
                    continue;
                }
                let config = Arc::clone(&config);
                let stop = Arc::clone(&stop);
                workers.push(thread::spawn(move || {
                    let _guard = ConnectionGuard(&config.active_connections);
                    let _ = handle_connection(stream, remote, &stop, &config);
                }));
                workers.retain(|worker| !worker.is_finished());
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(25));
            }
            Err(_) => break,
        }
    }
    for worker in workers {
        let _ = worker.join();
    }
}

fn reject_busy(mut stream: TcpStream) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let _ = response(
        &mut stream,
        429,
        "application/json",
        json_error("连接数量过多").as_bytes(),
    );
}

struct ConnectionGuard<'a>(&'a AtomicUsize);

impl Drop for ConnectionGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn handle_connection(
    mut stream: TcpStream,
    remote: SocketAddr,
    stop: &Arc<AtomicBool>,
    config: &Arc<RuntimeConfig>,
) -> Result<(), ServerError> {
    stream.set_read_timeout(Some(Duration::from_millis(250)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    let mut marker = [0_u8; 4];
    if stream.peek(&mut marker)? == marker.len() && &marker == PEER_MAGIC {
        let Some(peer) = &config.peer else {
            return Ok(());
        };
        if let Err(error) = peer.accept(stream) {
            eprintln!("remagic-upload: incoming peer sync failed: {error}");
            config.status.message(&format!("设备同步失败：{error}"));
        }
        return Ok(());
    }
    let request = match read_request(&mut stream, stop) {
        Ok(request) => request,
        Err(error) => {
            response(
                &mut stream,
                400,
                "application/json",
                json_error(&error.to_string()).as_bytes(),
            )?;
            return Ok(());
        }
    };
    if !same_origin(&request) {
        response(
            &mut stream,
            403,
            "application/json",
            json_error("来源不受信任").as_bytes(),
        )?;
        return Ok(());
    }
    route(stream, remote.ip(), request, stop, config)
}

fn route(
    mut stream: TcpStream,
    remote: IpAddr,
    request: HttpRequest,
    stop: &Arc<AtomicBool>,
    config: &Arc<RuntimeConfig>,
) -> Result<(), ServerError> {
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            INDEX.as_bytes(),
        )?,
        ("GET", "/app.js") => response(
            &mut stream,
            200,
            "text/javascript; charset=utf-8",
            SCRIPT.as_bytes(),
        )?,
        ("GET", "/style.css") => response(
            &mut stream,
            200,
            "text/css; charset=utf-8",
            STYLE.as_bytes(),
        )?,
        ("POST", "/api/session") => session(&mut stream, remote, request, config)?,
        ("GET", "/api/status") if authorized(&request, config) => {
            let body = serde_json::to_vec(&config.status.snapshot())?;
            response(&mut stream, 200, "application/json", &body)?;
        }
        ("PUT", "/api/upload") if authorized(&request, config) => {
            upload(&mut stream, request, stop, config)?;
        }
        ("GET", "/api/status") | ("PUT", "/api/upload") => {
            response(
                &mut stream,
                401,
                "application/json",
                json_error("需要重新配对").as_bytes(),
            )?;
        }
        _ => response(
            &mut stream,
            404,
            "application/json",
            json_error("不存在").as_bytes(),
        )?,
    }
    Ok(())
}

fn session(
    stream: &mut TcpStream,
    remote: IpAddr,
    request: HttpRequest,
    config: &RuntimeConfig,
) -> Result<(), ServerError> {
    if blocked(remote, config) {
        response(
            stream,
            429,
            "application/json",
            json_error("尝试次数过多，请稍后再试").as_bytes(),
        )?;
        return Ok(());
    }
    if request.content_length > 512 {
        response(
            stream,
            413,
            "application/json",
            json_error("请求过大").as_bytes(),
        )?;
        return Ok(());
    }
    let body = read_body(request, stream)?;
    let value: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => {
            record_failure(remote, config);
            response(
                stream,
                400,
                "application/json",
                json_error("配对请求格式错误").as_bytes(),
            )?;
            return Ok(());
        }
    };
    let pin = value.get("pin").and_then(serde_json::Value::as_str);
    let bootstrap = value.get("bootstrap").and_then(serde_json::Value::as_str);
    if !config.credentials.authorizes_session(pin, bootstrap) {
        record_failure(remote, config);
        response(
            stream,
            401,
            "application/json",
            json_error("配对码错误").as_bytes(),
        )?;
        return Ok(());
    }
    config.attempts.lock().unwrap().remove(&remote);
    let body = serde_json::to_vec(&serde_json::json!({
        "token": config.credentials.bearer,
        "expires": "foreground"
    }))?;
    response(stream, 200, "application/json", &body)?;
    Ok(())
}

fn upload(
    stream: &mut TcpStream,
    request: HttpRequest,
    stop: &Arc<AtomicBool>,
    config: &Arc<RuntimeConfig>,
) -> Result<(), ServerError> {
    if config
        .active_upload
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        response(
            stream,
            409,
            "application/json",
            json_error("已有文件正在上传").as_bytes(),
        )?;
        return Ok(());
    }
    let _guard = ActiveUploadGuard(&config.active_upload);
    let kind = request
        .headers
        .get("x-upload-kind")
        .and_then(|value| UploadKind::parse(value));
    let filename = request.headers.get("x-filename").cloned();
    let (Some(kind), Some(filename)) = (kind, filename) else {
        response(
            stream,
            400,
            "application/json",
            json_error("缺少文件类型或文件名").as_bytes(),
        )?;
        return Ok(());
    };
    let mut body = Cursor::new(request.body_prefix).chain(stream.try_clone()?);
    match config.registry.receive(
        kind,
        &filename,
        request.content_length,
        &mut body,
        stop,
        &config.status,
    ) {
        Ok(path) => {
            let saved = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("文件")
                .to_owned();
            config.status.complete(&saved);
            let body = serde_json::to_vec(&serde_json::json!({"saved_as": saved}))?;
            response(stream, 201, "application/json", &body)?;
        }
        Err(error) => {
            config.status.fail(&filename, &error.to_string());
            response(
                stream,
                422,
                "application/json",
                json_error(&error.to_string()).as_bytes(),
            )?;
        }
    }
    Ok(())
}

struct ActiveUploadGuard<'a>(&'a AtomicBool);

impl Drop for ActiveUploadGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn read_body(request: HttpRequest, stream: &mut TcpStream) -> Result<Vec<u8>, ServerError> {
    let prefix_len = request.body_prefix_len;
    let mut body = request.body_prefix;
    body.resize(request.content_length as usize, 0);
    stream.read_exact(&mut body[prefix_len..])?;
    Ok(body)
}

fn authorized(request: &HttpRequest, config: &RuntimeConfig) -> bool {
    config
        .credentials
        .authorizes_bearer(request.headers.get("authorization").map(String::as_str))
}

fn same_origin(request: &HttpRequest) -> bool {
    match (request.headers.get("origin"), request.headers.get("host")) {
        (Some(origin), Some(host)) => origin == &format!("http://{host}"),
        (Some(_), None) => false,
        _ => true,
    }
}

fn blocked(remote: IpAddr, config: &RuntimeConfig) -> bool {
    let mut attempts = config.attempts.lock().unwrap();
    let Some(window) = attempts.get(&remote) else {
        return false;
    };
    if window.since.elapsed() > Duration::from_secs(600) {
        attempts.remove(&remote);
        false
    } else {
        window.failures >= 5
    }
}

fn record_failure(remote: IpAddr, config: &RuntimeConfig) {
    let mut attempts = config.attempts.lock().unwrap();
    let window = attempts.entry(remote).or_insert(AttemptWindow {
        since: Instant::now(),
        failures: 0,
    });
    window.failures = window.failures.saturating_add(1);
}

fn json_error(message: &str) -> String {
    serde_json::json!({"error": message}).to_string()
}

#[derive(Debug, Error)]
pub enum ServerError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Upload(#[from] crate::upload::UploadError),
}
