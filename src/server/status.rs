use serde::Serialize;
use std::sync::Mutex;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RecentUpload {
    pub filename: String,
    pub message: String,
    pub success: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct StatusSnapshot {
    pub active: bool,
    pub filename: String,
    pub received: u64,
    pub total: u64,
    pub message: String,
    pub recent: Vec<RecentUpload>,
}

pub struct SharedStatus {
    inner: Mutex<StatusSnapshot>,
}

impl SharedStatus {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(StatusSnapshot {
                active: false,
                filename: String::new(),
                received: 0,
                total: 0,
                message: "等待上传".into(),
                recent: Vec::new(),
            }),
        }
    }

    pub fn begin(&self, filename: &str, total: u64) {
        let mut state = self.inner.lock().unwrap();
        state.active = true;
        state.filename = filename.into();
        state.received = 0;
        state.total = total;
        state.message = "正在接收".into();
    }

    pub fn activity(&self, label: &str, message: &str) {
        let mut state = self.inner.lock().unwrap();
        state.active = true;
        state.filename = label.into();
        state.received = 0;
        state.total = 0;
        state.message = message.into();
    }

    pub fn message(&self, message: &str) {
        let mut state = self.inner.lock().unwrap();
        state.active = false;
        state.filename.clear();
        state.received = 0;
        state.total = 0;
        state.message = message.into();
    }

    pub fn progress(&self, received: u64) {
        self.inner.lock().unwrap().received = received;
    }

    pub fn complete(&self, filename: &str) {
        let mut state = self.inner.lock().unwrap();
        state.active = false;
        state.received = state.total;
        state.message = "上传完成".into();
        push_recent(&mut state, filename, "已保存", true);
    }

    pub fn fail(&self, fallback_filename: &str, message: &str) {
        let mut state = self.inner.lock().unwrap();
        let filename = if state.active && !state.filename.is_empty() {
            state.filename.clone()
        } else {
            fallback_filename.into()
        };
        state.active = false;
        state.message = message.into();
        push_recent(&mut state, &filename, message, false);
    }

    pub fn snapshot(&self) -> StatusSnapshot {
        self.inner.lock().unwrap().clone()
    }
}

fn push_recent(state: &mut StatusSnapshot, filename: &str, message: &str, success: bool) {
    state.recent.insert(
        0,
        RecentUpload {
            filename: filename.into(),
            message: message.into(),
            success,
        },
    );
    state.recent.truncate(6);
}
