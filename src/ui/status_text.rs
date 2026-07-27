use crate::server::StatusSnapshot;

pub(super) fn status_title(status: &StatusSnapshot) -> String {
    if let Some(sync) = &status.sync {
        if status.active && !status.filename.is_empty() && sync.total_files > 0 {
            let current = sync.completed_files.saturating_add(1).min(sync.total_files);
            return format!(
                "{}：第 {current}/{} 个 {}",
                status.message, sync.total_files, status.filename
            );
        }
        if sync.total_files > 0 {
            return format!(
                "本次同步：接收 {}，发送 {}，删除 {}，共 {} 个文件",
                sync.receive_files, sync.send_files, sync.delete_files, sync.total_files
            );
        }
        if status.active {
            return "正在检查同步内容".into();
        }
    }
    if status.active && status.total > 0 {
        format!("正在上传：{}", status.filename)
    } else if status.active {
        format!("{}：{}", status.message, status.filename)
    } else {
        status.message.clone()
    }
}

pub(super) fn status_bar_ratio(status: &StatusSnapshot) -> Option<f64> {
    if let Some(sync) = &status.sync {
        if sync.total_bytes > 0 {
            return Some(
                sync.completed_bytes.min(sync.total_bytes) as f64 / sync.total_bytes as f64,
            );
        }
        if sync.total_files > 0 {
            return Some(
                sync.completed_files.min(sync.total_files) as f64 / sync.total_files as f64,
            );
        }
        return Some(0.0);
    }
    (status.total > 0).then(|| status.received.min(status.total) as f64 / status.total as f64)
}

pub(super) fn status_detail_lines(status: &StatusSnapshot) -> Vec<String> {
    if let Some(sync) = &status.sync {
        let mut lines = Vec::new();
        lines.push(format!(
            "统计：接收 {}，发送 {}，删除 {}",
            sync.receive_files, sync.send_files, sync.delete_files
        ));
        if sync.total_bytes > 0 {
            lines.push(format!(
                "总进度：{} / {}，文件 {}/{}",
                format_bytes(sync.completed_bytes),
                format_bytes(sync.total_bytes),
                sync.completed_files.min(sync.total_files),
                sync.total_files
            ));
        } else {
            lines.push(format!(
                "总进度：文件 {}/{}",
                sync.completed_files.min(sync.total_files),
                sync.total_files
            ));
        }
        if status.active && status.total > 0 {
            lines.push(format!(
                "当前文件：{} / {}",
                format_bytes(status.received),
                format_bytes(status.total)
            ));
        } else if !sync.preview.is_empty() {
            lines.push(format!("包含：{}", sync.preview.join("、")));
        }
        return lines;
    }
    status
        .recent
        .first()
        .map(|recent| vec![format!("最近：{}　{}", recent.filename, recent.message)])
        .unwrap_or_default()
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let value = bytes as f64;
    if value >= GIB {
        format!("{:.1} GB", value / GIB)
    } else if value >= MIB {
        format!("{:.1} MB", value / MIB)
    } else if value >= KIB {
        format!("{:.1} KB", value / KIB)
    } else {
        format!("{bytes} B")
    }
}
