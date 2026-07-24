use crate::auth::Credentials;
use crate::network::local_urls;
use crate::peer::{DiscoveryService, PeerRuntime};
use crate::server::{ServerConfig, ServerHandle, SharedStatus, StatusSnapshot};
use crate::ui::{ScreenModel, UploadUi};
use crate::upload::UploadRegistry;
use crate::{catalog::Catalog, peer::DiscoveredPeer};
use remagic_app_sdk::{
    LifecycleClient, LifecycleCommand, LifecycleStage, ManagedEnvironment, QtfbClient, Surface,
    TouchPhase, REFRESH_FAST, REFRESH_UI,
};
use std::error::Error;
use std::io;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub fn run() -> Result<(), Box<dyn Error>> {
    let environment = ManagedEnvironment::discover("upload")?;
    environment.require_transfer_contract()?;
    let bind = environment.listen_addr.expect("upload contract checked");
    let books_dir = environment
        .books_dir
        .clone()
        .expect("upload contract checked");
    let wallpapers_dir = environment
        .wallpapers_dir
        .clone()
        .expect("upload contract checked");
    let mut lifecycle =
        LifecycleClient::from_inherited_fd(environment.app_id.clone(), environment.lifecycle_fd)?;
    let mut qtfb = QtfbClient::connect(
        &environment.qtfb_socket,
        environment.qtfb_key,
        &environment.device.display,
    )?;
    let mut ui = UploadUi::load()?;
    let status = Arc::new(SharedStatus::new());
    let device_name = if environment.device.codename == "ferrari" {
        "Paper Pro"
    } else {
        "Paper Pro Move"
    };
    let catalog = Arc::new(Catalog::open(&environment.data_home, device_name)?);
    let storage = crate::peer::storage::PeerStorage::new(
        books_dir.clone(),
        wallpapers_dir.clone(),
        Arc::clone(&catalog),
        &environment.data_home,
    )?;
    let reading = crate::peer::reading::ReadingProvider::new(
        environment.control_socket.clone(),
        environment.app_id.clone(),
        &environment.data_home,
    );
    let peer_runtime = Arc::new(PeerRuntime::new(
        Arc::clone(&catalog),
        storage,
        reading,
        Arc::clone(&status),
    ));
    let mut state = AppState {
        bind,
        books_dir,
        wallpapers_dir,
        urls: Vec::new(),
        credentials: None,
        server: None,
        status,
        data_home: environment.data_home,
        catalog,
        peer_runtime,
        discovery: None,
        peers: Vec::new(),
        sync_worker: None,
        foreground: false,
        refresh_pressed: false,
        sync_pressed: false,
        primary_touch: None,
        frame_sequence: 0,
        last_snapshot: None,
        last_status_paint: Instant::now(),
    };

    loop {
        for command in lifecycle.poll()? {
            match command {
                LifecycleCommand::Start { .. } | LifecycleCommand::EnterForeground { .. } => {
                    state.enter_foreground()?;
                    render(&mut state, &mut ui, &mut qtfb, Damage::All)?;
                    lifecycle.ready(state.frame_sequence)?;
                }
                LifecycleCommand::EnterBackground => {
                    state.leave_foreground();
                    lifecycle.background_ready("文件传输", "已暂停，端口已关闭")?;
                }
                LifecycleCommand::Shutdown { .. } => {
                    state.leave_foreground();
                    lifecycle.shutdown_complete(0)?;
                    return Ok(());
                }
                LifecycleCommand::OpenPath { .. } => {
                    lifecycle.failed(
                        LifecycleStage::Foreground,
                        "文件传输不接受设备端文件路径",
                        false,
                    )?;
                }
            }
        }

        if state.foreground {
            let events = qtfb.drain_touch()?;
            for event in events {
                match event.phase {
                    TouchPhase::Press if state.primary_touch.is_none() => {
                        state.primary_touch = Some(event.finger);
                        if ui.refresh_button.contains(event.x, event.y) {
                            state.refresh_pressed = true;
                            render(&mut state, &mut ui, &mut qtfb, Damage::Button)?;
                        } else if ui.sync_button.contains(event.x, event.y) {
                            state.sync_pressed = true;
                            render(&mut state, &mut ui, &mut qtfb, Damage::Button)?;
                        }
                    }
                    TouchPhase::Release if state.primary_touch == Some(event.finger) => {
                        let activate =
                            state.refresh_pressed && ui.refresh_button.contains(event.x, event.y);
                        let activate_sync =
                            state.sync_pressed && ui.sync_button.contains(event.x, event.y);
                        state.primary_touch = None;
                        state.refresh_pressed = false;
                        state.sync_pressed = false;
                        if activate {
                            state.rotate_server()?;
                            render(&mut state, &mut ui, &mut qtfb, Damage::All)?;
                        } else if activate_sync {
                            state.activate_sync();
                            render(&mut state, &mut ui, &mut qtfb, Damage::Status)?;
                        } else {
                            render(&mut state, &mut ui, &mut qtfb, Damage::Button)?;
                        }
                    }
                    _ => {}
                }
            }
            let peers_changed = state.poll_discovery();
            state.reap_worker();
            let snapshot = state.status.snapshot();
            if (peers_changed || state.last_snapshot.as_ref() != Some(&snapshot))
                && state.last_status_paint.elapsed() >= Duration::from_millis(250)
            {
                state.last_snapshot = Some(snapshot);
                state.last_status_paint = Instant::now();
                render(&mut state, &mut ui, &mut qtfb, Damage::Status)?;
            }
        }
        qtfb.wait(Duration::from_millis(25))?;
    }
}

struct AppState {
    bind: std::net::SocketAddr,
    books_dir: std::path::PathBuf,
    wallpapers_dir: std::path::PathBuf,
    urls: Vec<String>,
    credentials: Option<Credentials>,
    server: Option<ServerHandle>,
    status: Arc<SharedStatus>,
    data_home: std::path::PathBuf,
    catalog: Arc<Catalog>,
    peer_runtime: Arc<PeerRuntime>,
    discovery: Option<DiscoveryService>,
    peers: Vec<DiscoveredPeer>,
    sync_worker: Option<JoinHandle<()>>,
    foreground: bool,
    refresh_pressed: bool,
    sync_pressed: bool,
    primary_touch: Option<i32>,
    frame_sequence: u64,
    last_snapshot: Option<StatusSnapshot>,
    last_status_paint: Instant,
}

impl AppState {
    fn enter_foreground(&mut self) -> Result<(), Box<dyn Error>> {
        if self.foreground {
            return Ok(());
        }
        self.foreground = true;
        self.rotate_server()?;
        match DiscoveryService::start(self.catalog.identity(), self.bind.port()) {
            Ok(service) => self.discovery = Some(service),
            Err(error) => {
                eprintln!("remagic-upload: LAN discovery failed: {error}");
                self.status.message(&format!("局域网发现未启动：{error}"));
            }
        }
        Ok(())
    }

    fn rotate_server(&mut self) -> Result<(), Box<dyn Error>> {
        if let Some(server) = self.server.take() {
            server.stop();
        }
        let credentials = Credentials::generate()?;
        self.urls = local_urls(self.bind.port())?;
        let registry = UploadRegistry::managed(
            self.books_dir.clone(),
            self.wallpapers_dir.clone(),
            Arc::clone(&self.catalog),
            &self.data_home,
        )?;
        self.server = Some(ServerHandle::start(ServerConfig {
            bind: self.bind,
            credentials: credentials.clone(),
            registry,
            status: Arc::clone(&self.status),
            peer: Some(Arc::clone(&self.peer_runtime)),
        })?);
        self.credentials = Some(credentials);
        Ok(())
    }

    fn leave_foreground(&mut self) {
        self.foreground = false;
        self.primary_touch = None;
        self.refresh_pressed = false;
        self.sync_pressed = false;
        self.discovery = None;
        self.peers.clear();
        self.credentials = None;
        if let Some(server) = self.server.take() {
            server.stop();
        }
        if let Some(worker) = self.sync_worker.take() {
            let _ = worker.join();
        }
    }

    fn poll_discovery(&mut self) -> bool {
        let Some(discovery) = &mut self.discovery else {
            return false;
        };
        discovery.poll();
        let peers = discovery.peers();
        if peers == self.peers {
            return false;
        }
        self.peers = peers;
        true
    }

    fn activate_sync(&mut self) {
        if self.sync_worker.is_some() {
            self.status.message("同步正在进行");
            return;
        }
        let Some(peer) = self.peers.first().cloned() else {
            self.status.message("未发现另一台已打开“文件传输”的设备");
            return;
        };
        match self.peer_runtime.is_trusted(&peer) {
            Ok(false) => {
                if let Err(error) = self.peer_runtime.trust(&peer) {
                    self.status.message(&format!("配对失败：{error}"));
                }
            }
            Ok(true) => {
                let runtime = Arc::clone(&self.peer_runtime);
                let status = Arc::clone(&self.status);
                self.sync_worker = Some(std::thread::spawn(move || {
                    if let Err(error) = runtime.synchronize(&peer) {
                        eprintln!("remagic-upload: peer sync failed: {error}");
                        status.message(&format!("同步失败：{}", error.user_message()));
                    }
                }));
            }
            Err(error) => self.status.message(&format!("无法检查配对状态：{error}")),
        }
    }

    fn reap_worker(&mut self) {
        if self
            .sync_worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            let worker = self.sync_worker.take().expect("checked");
            let _ = worker.join();
        }
    }
}

fn render(
    state: &mut AppState,
    ui: &mut UploadUi,
    qtfb: &mut QtfbClient,
    damage: Damage,
) -> Result<(), Box<dyn Error>> {
    let credentials = state
        .credentials
        .as_ref()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "server is not active"))?;
    let primary = state
        .urls
        .first()
        .cloned()
        .unwrap_or_else(|| format!("http://10.11.99.1:{}", state.bind.port()));
    let qr = format!("{primary}/#{}", credentials.bootstrap);
    let snapshot = state.status.snapshot();
    let width = qtfb.width;
    let height = qtfb.height;
    let stride = qtfb.stride;
    let framebuffer = qtfb.framebuffer();
    let mut surface = Surface::new(framebuffer, width, height, stride)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "invalid QTFB surface"))?;
    ui.render(
        &mut surface,
        &ScreenModel {
            urls: &state.urls,
            pin: &credentials.pin,
            qr_content: &qr,
            status: &snapshot,
            refresh_pressed: state.refresh_pressed,
            sync_pressed: state.sync_pressed,
            peer: state.peers.first(),
            peer_trusted: state
                .peers
                .first()
                .and_then(|peer| state.peer_runtime.is_trusted(peer).ok())
                .unwrap_or(false),
        },
    );
    state.frame_sequence = state.frame_sequence.saturating_add(1).max(1);
    match damage {
        Damage::All => qtfb.update_all(REFRESH_UI)?,
        Damage::Status => {
            let rect = ui.status_region;
            qtfb.update_partial(
                rect.x as i32,
                rect.y as i32,
                rect.width as i32,
                rect.height as i32,
                REFRESH_FAST,
            )?;
        }
        Damage::Button => {
            let rect = ui.button_region();
            qtfb.update_partial(
                rect.x as i32,
                rect.y as i32,
                rect.width as i32,
                rect.height as i32,
                REFRESH_FAST,
            )?;
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Damage {
    All,
    Status,
    Button,
}
