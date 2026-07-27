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
    pub sync: Option<SyncStatus>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SyncStatus {
    pub send_files: u64,
    pub receive_files: u64,
    pub delete_files: u64,
    pub total_files: u64,
    pub completed_files: u64,
    pub total_bytes: u64,
    pub completed_bytes: u64,
    pub preview: Vec<String>,
    #[serde(skip_serializing)]
    current_base_bytes: u64,
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
                sync: None,
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
        state.sync = None;
    }

    pub fn activity(&self, label: &str, message: &str) {
        let mut state = self.inner.lock().unwrap();
        state.active = true;
        state.filename = label.into();
        state.received = 0;
        state.total = 0;
        state.message = message.into();
        state.sync = None;
    }

    pub fn sync_plan(
        &self,
        send_files: usize,
        receive_files: usize,
        delete_files: usize,
        total_bytes: u64,
        preview: Vec<String>,
    ) {
        let mut state = self.inner.lock().unwrap();
        state.active = true;
        state.received = 0;
        state.total = 0;
        state.message = "准备同步".into();
        let total_files = send_files
            .saturating_add(receive_files)
            .saturating_add(delete_files) as u64;
        state.sync = Some(SyncStatus {
            send_files: send_files as u64,
            receive_files: receive_files as u64,
            delete_files: delete_files as u64,
            total_files,
            completed_files: 0,
            total_bytes,
            completed_bytes: 0,
            preview,
            current_base_bytes: 0,
        });
    }

    pub fn sync_transfer(&self, label: &str, total: u64, message: &str) {
        let mut state = self.inner.lock().unwrap();
        state.active = true;
        state.filename = label.into();
        state.received = 0;
        state.total = total;
        state.message = message.into();
        if let Some(sync) = &mut state.sync {
            sync.current_base_bytes = sync.completed_bytes;
        }
    }

    pub fn message(&self, message: &str) {
        let mut state = self.inner.lock().unwrap();
        state.active = false;
        state.filename.clear();
        state.received = 0;
        state.total = 0;
        state.message = message.into();
        state.sync = None;
    }

    pub fn progress(&self, received: u64) {
        let mut state = self.inner.lock().unwrap();
        state.received = received;
        if let Some(sync) = &mut state.sync {
            sync.completed_bytes = sync
                .current_base_bytes
                .saturating_add(received)
                .min(sync.total_bytes);
        }
    }

    pub fn sync_file_done(&self) {
        let mut state = self.inner.lock().unwrap();
        state.received = state.total;
        let total = state.total;
        if let Some(sync) = &mut state.sync {
            sync.completed_files = sync.completed_files.saturating_add(1).min(sync.total_files);
            sync.completed_bytes = sync
                .current_base_bytes
                .saturating_add(total)
                .min(sync.total_bytes);
            sync.current_base_bytes = sync.completed_bytes;
        }
    }

    pub fn sync_complete(&self, message: &str) {
        let mut state = self.inner.lock().unwrap();
        state.active = false;
        state.filename.clear();
        state.received = 0;
        state.total = 0;
        state.message = message.into();
        if let Some(sync) = &mut state.sync {
            sync.completed_files = sync.total_files;
            sync.completed_bytes = sync.total_bytes;
            sync.current_base_bytes = sync.total_bytes;
        }
    }

    pub fn complete(&self, filename: &str) {
        let mut state = self.inner.lock().unwrap();
        state.active = false;
        state.received = state.total;
        state.message = "上传完成".into();
        state.sync = None;
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
        if let Some(sync) = &mut state.sync {
            sync.current_base_bytes = sync.completed_bytes;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_progress_tracks_current_file_and_overall_bytes() {
        let status = SharedStatus::new();
        status.sync_plan(1, 1, 1, 30, vec!["a.epub".into()]);
        status.sync_transfer("a.epub", 10, "正在发送");
        status.progress(4);
        let snapshot = status.snapshot();
        assert_eq!(snapshot.received, 4);
        assert_eq!(snapshot.sync.as_ref().unwrap().completed_bytes, 4);

        status.progress(10);
        status.sync_file_done();
        status.sync_transfer("b.pdf", 20, "正在接收");
        status.progress(5);
        let snapshot = status.snapshot();
        let sync = snapshot.sync.unwrap();
        assert_eq!(sync.completed_files, 1);
        assert_eq!(sync.completed_bytes, 15);
        assert_eq!(sync.total_files, 3);
    }

    #[test]
    fn ordinary_messages_clear_sync_state() {
        let status = SharedStatus::new();
        status.sync_plan(1, 0, 0, 10, Vec::new());
        status.message("等待上传");
        assert!(status.snapshot().sync.is_none());
    }
}
