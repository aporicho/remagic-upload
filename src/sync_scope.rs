use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;

const SETTINGS_FILE: &str = "sync-settings.json";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncItem {
    Books,
    Koreader,
    KoreaderFontSize,
    Magicpaper,
    Wallpapers,
    AiKeys,
}

impl SyncItem {
    pub const ALL: [SyncItem; 6] = [
        SyncItem::Books,
        SyncItem::Koreader,
        SyncItem::KoreaderFontSize,
        SyncItem::Magicpaper,
        SyncItem::Wallpapers,
        SyncItem::AiKeys,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SyncItem::Books => "书籍",
            SyncItem::Koreader => "KOReader",
            SyncItem::KoreaderFontSize => "字体大小",
            SyncItem::Magicpaper => "MagicPaper",
            SyncItem::Wallpapers => "壁纸",
            SyncItem::AiKeys => "AI 密钥",
        }
    }

    pub fn summary(self) -> &'static str {
        match self {
            SyncItem::Books => "书籍",
            SyncItem::Koreader => "KOReader 数据",
            SyncItem::KoreaderFontSize => "字体大小",
            SyncItem::Magicpaper => "MagicPaper 数据",
            SyncItem::Wallpapers => "壁纸",
            SyncItem::AiKeys => "AI 密钥",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncSelection {
    #[serde(default = "enabled")]
    pub books: bool,
    #[serde(default = "enabled")]
    pub koreader: bool,
    #[serde(default = "disabled")]
    pub koreader_font_size: bool,
    #[serde(default = "enabled")]
    pub magicpaper: bool,
    #[serde(default = "enabled")]
    pub wallpapers: bool,
    #[serde(default = "enabled")]
    pub ai_keys: bool,
}

impl Default for SyncSelection {
    fn default() -> Self {
        Self {
            books: true,
            koreader: true,
            koreader_font_size: false,
            magicpaper: true,
            wallpapers: true,
            ai_keys: true,
        }
    }
}

impl SyncSelection {
    pub fn load_or_default(data_home: &Path) -> Self {
        match fs::read(data_home.join(SETTINGS_FILE)) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self, data_home: &Path) -> io::Result<()> {
        fs::create_dir_all(data_home)?;
        let temporary = data_home.join(format!(".{SETTINGS_FILE}.tmp"));
        let path = data_home.join(SETTINGS_FILE);
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, path)
    }

    pub fn contains(&self, item: SyncItem) -> bool {
        match item {
            SyncItem::Books => self.books,
            SyncItem::Koreader => self.koreader,
            SyncItem::KoreaderFontSize => self.koreader_font_size,
            SyncItem::Magicpaper => self.magicpaper,
            SyncItem::Wallpapers => self.wallpapers,
            SyncItem::AiKeys => self.ai_keys,
        }
    }

    pub fn set(&mut self, item: SyncItem, enabled: bool) {
        match item {
            SyncItem::Books => self.books = enabled,
            SyncItem::Koreader => self.koreader = enabled,
            SyncItem::KoreaderFontSize => self.koreader_font_size = enabled,
            SyncItem::Magicpaper => self.magicpaper = enabled,
            SyncItem::Wallpapers => self.wallpapers = enabled,
            SyncItem::AiKeys => self.ai_keys = enabled,
        }
    }

    pub fn toggle(&mut self, item: SyncItem) {
        self.set(item, !self.contains(item));
    }

    pub fn any(&self) -> bool {
        SyncItem::ALL
            .iter()
            .copied()
            .any(|item| self.contains(item))
    }

    pub fn summaries(&self) -> Vec<&'static str> {
        SyncItem::ALL
            .iter()
            .copied()
            .filter(|item| self.contains(*item))
            .map(SyncItem::summary)
            .collect()
    }
}

fn enabled() -> bool {
    true
}

fn disabled() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_selects_core_items_without_font_size() {
        let selection = SyncSelection::default();
        assert!(selection.books);
        assert!(selection.koreader);
        assert!(!selection.koreader_font_size);
        assert!(selection.magicpaper);
        assert!(selection.wallpapers);
        assert!(selection.ai_keys);
        assert!(selection.any());
    }

    #[test]
    fn missing_new_fields_keep_safe_defaults() {
        let selection: SyncSelection = serde_json::from_slice(br#"{"books":false}"#).unwrap();
        assert!(!selection.books);
        assert!(selection.koreader);
        assert!(!selection.koreader_font_size);
        assert!(selection.magicpaper);
        assert!(selection.wallpapers);
        assert!(selection.ai_keys);
    }
}
