use crate::sync_scope::{SyncItem, SyncSelection};
use std::path::{Component, Path, PathBuf};

pub(super) struct SyncRoot {
    pub(super) kind: &'static str,
    pub(super) path: PathBuf,
    filter: Filter,
}

impl SyncRoot {
    fn new(kind: &'static str, path: PathBuf, filter: Filter) -> Self {
        Self { kind, path, filter }
    }

    pub(super) fn accepts(&self, relative: &str) -> bool {
        self.filter.accepts(relative)
    }
}

#[derive(Clone, Copy)]
pub(super) enum Filter {
    All,
    Book,
    Wallpaper,
    Exact(&'static str),
    KoreaderSidecar,
    MagicpaperConfig,
}

impl Filter {
    pub(super) fn accepts(self, relative: &str) -> bool {
        match self {
            Filter::All => true,
            Filter::Book => is_supported_book(relative),
            Filter::Wallpaper => extension(relative) == "png",
            Filter::Exact(expected) => relative == expected,
            Filter::KoreaderSidecar => is_koreader_sidecar(relative),
            Filter::MagicpaperConfig => !has_component(relative, "oracle.env"),
        }
    }
}

pub(super) fn roots_for(
    home: &Path,
    books: &Path,
    wallpapers: &Path,
    selection: &SyncSelection,
) -> Vec<SyncRoot> {
    let mut roots = Vec::new();
    if selection.contains(SyncItem::Books) {
        roots.push(SyncRoot::new("book", books.to_path_buf(), Filter::Book));
    }
    if selection.contains(SyncItem::Koreader) {
        roots.extend([
            SyncRoot::new(
                "koreader_data",
                home.join(".local/share/koreader-for-remagic/data"),
                Filter::All,
            ),
            SyncRoot::new(
                "koreader_legacy_data",
                home.join(".local/share/remagic-koreader/data"),
                Filter::All,
            ),
            SyncRoot::new(
                "koreader_sidecar",
                books.to_path_buf(),
                Filter::KoreaderSidecar,
            ),
        ]);
    }
    if selection.contains(SyncItem::Magicpaper) {
        roots.extend([
            SyncRoot::new(
                "magicpaper_data",
                home.join(".local/share/magicpaper"),
                Filter::All,
            ),
            SyncRoot::new(
                "magicpaper_config",
                home.join(".config/magicpaper"),
                Filter::MagicpaperConfig,
            ),
            SyncRoot::new(
                "magicpaper_legacy_data",
                home.join(".local/share/remagic-magicpaper"),
                Filter::All,
            ),
            SyncRoot::new(
                "magicpaper_legacy_config",
                home.join(".config/remagic-magicpaper"),
                Filter::MagicpaperConfig,
            ),
            SyncRoot::new(
                "magicpaper_riddle_data",
                home.join("riddle-data"),
                Filter::All,
            ),
            SyncRoot::new(
                "magicpaper_riddle_config",
                home.join(".config/riddle"),
                Filter::MagicpaperConfig,
            ),
        ]);
    }
    if selection.contains(SyncItem::Wallpapers) {
        roots.extend([
            SyncRoot::new("wallpaper", wallpapers.to_path_buf(), Filter::Wallpaper),
            SyncRoot::new(
                "home_settings",
                home.join(".config/remagic"),
                Filter::Exact("home.toml"),
            ),
        ]);
    }
    if selection.contains(SyncItem::AiKeys) {
        roots.extend([
            SyncRoot::new(
                "remagic_secret",
                home.join(".config/remagic/secrets/providers"),
                Filter::All,
            ),
            SyncRoot::new(
                "magicpaper_oracle_secret",
                home.join(".config/magicpaper"),
                Filter::Exact("oracle.env"),
            ),
        ]);
    }
    roots
}

pub(super) fn root_for_kind(
    home: &Path,
    books: &Path,
    wallpapers: &Path,
    kind: &str,
) -> Option<PathBuf> {
    match kind {
        "book" => Some(books.to_path_buf()),
        "wallpaper" => Some(wallpapers.to_path_buf()),
        "home_settings" => Some(home.join(".config/remagic")),
        "koreader_data" => Some(home.join(".local/share/koreader-for-remagic/data")),
        "koreader_legacy_data" => Some(home.join(".local/share/remagic-koreader/data")),
        "koreader_sidecar" => Some(books.to_path_buf()),
        "magicpaper_data" => Some(home.join(".local/share/magicpaper")),
        "magicpaper_config" => Some(home.join(".config/magicpaper")),
        "magicpaper_legacy_data" => Some(home.join(".local/share/remagic-magicpaper")),
        "magicpaper_legacy_config" => Some(home.join(".config/remagic-magicpaper")),
        "magicpaper_riddle_data" => Some(home.join("riddle-data")),
        "magicpaper_riddle_config" => Some(home.join(".config/riddle")),
        "remagic_secret" => Some(home.join(".config/remagic/secrets/providers")),
        "magicpaper_oracle_secret" => Some(home.join(".config/magicpaper")),
        _ => None,
    }
}

pub(super) fn filter_for_kind(kind: &str) -> Option<Filter> {
    match kind {
        "book" => Some(Filter::Book),
        "wallpaper" => Some(Filter::Wallpaper),
        "home_settings" => Some(Filter::Exact("home.toml")),
        "koreader_data" | "koreader_legacy_data" => Some(Filter::All),
        "koreader_sidecar" => Some(Filter::KoreaderSidecar),
        "magicpaper_data"
        | "magicpaper_legacy_data"
        | "magicpaper_riddle_data"
        | "remagic_secret" => Some(Filter::All),
        "magicpaper_config" | "magicpaper_legacy_config" | "magicpaper_riddle_config" => {
            Some(Filter::MagicpaperConfig)
        }
        "magicpaper_oracle_secret" => Some(Filter::Exact("oracle.env")),
        _ => None,
    }
}

fn is_supported_book(path: &str) -> bool {
    matches!(
        extension(path).as_str(),
        "pdf" | "djvu" | "djv" | "mobi" | "azw3" | "fb2" | "epub" | "cbz" | "cbr" | "txt"
    )
}

fn is_koreader_sidecar(path: &str) -> bool {
    Path::new(path)
        .components()
        .any(|component| component_name(component).is_some_and(|name| name.ends_with(".sdr")))
}

fn has_component(path: &str, expected: &str) -> bool {
    Path::new(path)
        .components()
        .any(|component| component_name(component) == Some(expected))
}

fn component_name(component: Component<'_>) -> Option<&str> {
    component.as_os_str().to_str()
}

fn extension(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}
