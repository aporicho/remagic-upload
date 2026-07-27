use ab_glyph::{point, Font, FontArc, PxScale, ScaleFont};
use qrcode::{Color, QrCode};
use remagic_app_sdk::{Rgb565, Surface, BLACK, WHITE};
use std::fs;
use std::io;
use std::path::Path;

use crate::peer::DiscoveredPeer;
use crate::server::StatusSnapshot;
use crate::sync_scope::{SyncItem, SyncSelection};

mod status_text;
use status_text::{status_bar_ratio, status_detail_lines, status_title};

const UI_FONT: &str = "/home/root/apps/remagic/fonts/UIFont.ttf";
const GRAY: Rgb565 = 0x8410;

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl Rect {
    pub fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x as i32
            && y >= self.y as i32
            && x < self.x.saturating_add(self.width) as i32
            && y < self.y.saturating_add(self.height) as i32
    }
}

pub struct UploadUi {
    font: FontArc,
    pub refresh_button: Rect,
    pub sync_button: Rect,
    sync_item_buttons: Vec<(SyncItem, Rect)>,
    pub status_region: Rect,
}

pub struct ScreenModel<'a> {
    pub urls: &'a [String],
    pub pin: &'a str,
    pub qr_content: &'a str,
    pub status: &'a StatusSnapshot,
    pub refresh_pressed: bool,
    pub sync_pressed: bool,
    pub item_pressed: Option<SyncItem>,
    pub sync_selection: &'a SyncSelection,
    pub peer: Option<&'a DiscoveredPeer>,
    pub peer_trusted: bool,
}

impl UploadUi {
    pub fn load() -> io::Result<Self> {
        Self::load_at(Path::new(UI_FONT))
    }

    fn load_at(path: &Path) -> io::Result<Self> {
        let font = FontArc::try_from_vec(fs::read(path)?)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "UI font is invalid"))?;
        Ok(Self {
            font,
            refresh_button: Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
            sync_button: Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
            sync_item_buttons: Vec::new(),
            status_region: Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            },
        })
    }

    pub fn render(&mut self, surface: &mut Surface<'_>, model: &ScreenModel<'_>) {
        let width = surface.width();
        let height = surface.height();
        let margin = (width as f32 * 0.055).round() as usize;
        let unit = (width as f32 / 954.0).clamp(1.0, 1.70);
        self.sync_item_buttons.clear();
        surface.clear(WHITE);
        self.text(
            surface,
            "文件传输",
            margin as f32,
            margin as f32,
            54.0 * unit,
            BLACK,
        );
        self.text(
            surface,
            "浏览器上传，或与另一台 ReMagic 设备同步所选数据",
            margin as f32,
            margin as f32 + 75.0 * unit,
            25.0 * unit,
            GRAY,
        );
        surface.fill_rect(
            margin,
            (margin as f32 + 116.0 * unit) as usize,
            width.saturating_sub(margin * 2),
            (2.0 * unit).max(2.0) as usize,
            BLACK,
        );

        let narrow = width < 1200;
        let qr_size = if narrow {
            (width as f32 * 0.54) as usize
        } else {
            (width as f32 * 0.36) as usize
        };
        let qr_x = if narrow {
            (width - qr_size) / 2
        } else {
            width - margin - qr_size
        };
        let qr_y = if narrow {
            (margin as f32 + 330.0 * unit) as usize
        } else {
            (margin as f32 + 155.0 * unit) as usize
        };
        self.qr(surface, model.qr_content, qr_x, qr_y, qr_size);

        let mut y = margin as f32 + 155.0 * unit;
        let text_width = if narrow {
            width.saturating_sub(margin * 2)
        } else {
            width.saturating_sub(margin * 3 + qr_size)
        };
        self.text(surface, "访问地址", margin as f32, y, 27.0 * unit, GRAY);
        y += 44.0 * unit;
        if model.urls.is_empty() {
            self.text(
                surface,
                "未发现 USB 或 Wi-Fi 地址",
                margin as f32,
                y,
                27.0 * unit,
                BLACK,
            );
        } else {
            for url in model.urls.iter().take(3) {
                self.text_fit(
                    surface,
                    url,
                    (margin as f32, y),
                    text_width as f32,
                    31.0 * unit,
                    BLACK,
                );
                y += 46.0 * unit;
            }
        }
        y += 24.0 * unit;
        self.text(surface, "配对码", margin as f32, y, 27.0 * unit, GRAY);
        y += 43.0 * unit;
        self.text_fit(
            surface,
            model.pin,
            (margin as f32, y),
            text_width as f32,
            72.0 * unit,
            BLACK,
        );

        let status_y = if narrow {
            qr_y.saturating_add(qr_size)
                .saturating_add((45.0 * unit) as usize)
        } else {
            (height as f32 * 0.62) as usize
        };
        let status_title = status_title(model.status);
        self.text_fit(
            surface,
            &status_title,
            (margin as f32, status_y as f32),
            width.saturating_sub(margin * 2) as f32,
            31.0 * unit,
            BLACK,
        );
        let bar_y = status_y.saturating_add((53.0 * unit) as usize);
        surface.fill_rect(
            margin,
            bar_y,
            width - margin * 2,
            (8.0 * unit) as usize,
            0xd69a,
        );
        if let Some(ratio) = status_bar_ratio(model.status) {
            surface.fill_rect(
                margin,
                bar_y,
                ((width - margin * 2) as f64 * ratio) as usize,
                (8.0 * unit) as usize,
                BLACK,
            );
        }
        let detail_lines = status_detail_lines(model.status);
        let mut detail_y = bar_y as f32 + 30.0 * unit;
        for line in detail_lines.iter().take(3) {
            self.text_fit(
                surface,
                line,
                (margin as f32, detail_y),
                width.saturating_sub(margin * 2) as f32,
                24.0 * unit,
                GRAY,
            );
            detail_y += 32.0 * unit;
        }

        let peer_text = match model.peer {
            Some(peer) if model.peer_trusted => format!("设备同步：{}　已配对", peer.name),
            Some(peer) => format!("设备同步：{}　配对码 {}", peer.name, peer.pairing_code),
            None => "设备同步：等待局域网发现".to_owned(),
        };
        self.text_fit(
            surface,
            &peer_text,
            (margin as f32, detail_y + 4.0 * unit),
            width.saturating_sub(margin * 2) as f32,
            24.0 * unit,
            GRAY,
        );

        let options_y = (detail_y + 49.0 * unit) as usize;
        self.text(
            surface,
            "同步项",
            margin as f32,
            options_y as f32,
            24.0 * unit,
            GRAY,
        );
        let row_height = (49.0 * unit) as usize;
        let checkbox = (28.0 * unit).max(24.0) as usize;
        let mut row_y = options_y.saturating_add((37.0 * unit) as usize);
        for item in SyncItem::ALL {
            let rect = Rect {
                x: margin,
                y: row_y,
                width: width.saturating_sub(margin * 2),
                height: row_height,
            };
            self.sync_item_buttons.push((item, rect));
            let pressed = model.item_pressed == Some(item);
            if pressed {
                surface.fill_rect(rect.x, rect.y, rect.width, rect.height, 0xef7d);
            }
            let box_y = rect.y + rect.height.saturating_sub(checkbox) / 2;
            surface.stroke_rect(
                rect.x,
                box_y,
                checkbox,
                checkbox,
                (3.0 * unit).max(2.0) as usize,
                BLACK,
            );
            if model.sync_selection.contains(item) {
                let inset = (7.0 * unit).max(5.0) as usize;
                surface.fill_rect(
                    rect.x + inset,
                    box_y + inset,
                    checkbox.saturating_sub(inset * 2),
                    checkbox.saturating_sub(inset * 2),
                    BLACK,
                );
            }
            self.text(
                surface,
                item.label(),
                (rect.x + checkbox + (18.0 * unit) as usize) as f32,
                rect.y as f32 + (7.0 * unit),
                25.0 * unit,
                BLACK,
            );
            row_y = row_y.saturating_add(row_height);
        }

        let button_height = (92.0 * unit) as usize;
        let gap = (18.0 * unit) as usize;
        let available = width.saturating_sub(margin * 2 + gap);
        self.refresh_button = Rect {
            x: margin,
            y: height.saturating_sub(margin + button_height),
            width: available / 2,
            height: button_height,
        };
        self.sync_button = Rect {
            x: self.refresh_button.x + self.refresh_button.width + gap,
            y: self.refresh_button.y,
            width: available - self.refresh_button.width,
            height: button_height,
        };
        self.status_region = Rect {
            x: margin,
            y: status_y.saturating_sub((8.0 * unit) as usize),
            width: width.saturating_sub(margin * 2),
            height: self
                .refresh_button
                .y
                .saturating_sub(status_y)
                .saturating_sub((16.0 * unit) as usize),
        };
        let (background, foreground) = if model.refresh_pressed {
            (BLACK, WHITE)
        } else {
            (WHITE, BLACK)
        };
        surface.fill_rect(
            self.refresh_button.x,
            self.refresh_button.y,
            self.refresh_button.width,
            self.refresh_button.height,
            background,
        );
        surface.stroke_rect(
            self.refresh_button.x,
            self.refresh_button.y,
            self.refresh_button.width,
            self.refresh_button.height,
            (3.0 * unit).max(2.0) as usize,
            foreground,
        );
        self.text_centered(
            surface,
            "刷新上传码",
            self.refresh_button,
            30.0 * unit,
            foreground,
        );
        let (background, foreground) = if model.sync_pressed {
            (BLACK, WHITE)
        } else {
            (WHITE, BLACK)
        };
        surface.fill_rect(
            self.sync_button.x,
            self.sync_button.y,
            self.sync_button.width,
            self.sync_button.height,
            background,
        );
        surface.stroke_rect(
            self.sync_button.x,
            self.sync_button.y,
            self.sync_button.width,
            self.sync_button.height,
            (3.0 * unit).max(2.0) as usize,
            foreground,
        );
        self.text_centered(
            surface,
            if model.peer_trusted {
                "同步所选"
            } else {
                "确认配对"
            },
            self.sync_button,
            30.0 * unit,
            foreground,
        );
    }

    pub fn button_region(&self) -> Rect {
        Rect {
            x: self.refresh_button.x,
            y: self.refresh_button.y,
            width: self.sync_button.x + self.sync_button.width - self.refresh_button.x,
            height: self.refresh_button.height,
        }
    }

    pub fn sync_item_at(&self, x: i32, y: i32) -> Option<SyncItem> {
        self.sync_item_buttons
            .iter()
            .find_map(|(item, rect)| rect.contains(x, y).then_some(*item))
    }

    pub fn sync_item_contains(&self, item: SyncItem, x: i32, y: i32) -> bool {
        self.sync_item_buttons
            .iter()
            .find(|(candidate, _)| *candidate == item)
            .is_some_and(|(_, rect)| rect.contains(x, y))
    }

    fn qr(&self, surface: &mut Surface<'_>, content: &str, x: usize, y: usize, size: usize) {
        let Ok(code) = QrCode::new(content.as_bytes()) else {
            return;
        };
        let modules = code.width();
        let quiet = 4;
        let cell = (size / (modules + quiet * 2)).max(1);
        let actual = cell * (modules + quiet * 2);
        let ox = x + size.saturating_sub(actual) / 2;
        let oy = y + size.saturating_sub(actual) / 2;
        surface.fill_rect(ox, oy, actual, actual, WHITE);
        for row in 0..modules {
            for column in 0..modules {
                if code[(column, row)] == Color::Dark {
                    surface.fill_rect(
                        ox + (column + quiet) * cell,
                        oy + (row + quiet) * cell,
                        cell,
                        cell,
                        BLACK,
                    );
                }
            }
        }
    }

    fn text_fit(
        &self,
        surface: &mut Surface<'_>,
        text: &str,
        position: (f32, f32),
        maximum_width: f32,
        size: f32,
        color: Rgb565,
    ) {
        let width = self.measure(text, size);
        let fitted = if width > maximum_width {
            (size * maximum_width / width).max(14.0)
        } else {
            size
        };
        self.text(surface, text, position.0, position.1, fitted, color);
    }

    fn text_centered(
        &self,
        surface: &mut Surface<'_>,
        text: &str,
        rectangle: Rect,
        size: f32,
        color: Rgb565,
    ) {
        let width = self.measure(text, size);
        let x = rectangle.x as f32 + (rectangle.width as f32 - width).max(0.0) / 2.0;
        let y = rectangle.y as f32 + (rectangle.height as f32 - size) / 2.0 - size * 0.08;
        self.text(surface, text, x, y, size, color);
    }

    fn measure(&self, text: &str, size: f32) -> f32 {
        let scaled = self.font.as_scaled(PxScale::from(size));
        let mut width = 0.0;
        let mut previous = None;
        for character in text.chars() {
            let id = scaled.glyph_id(character);
            if let Some(previous) = previous {
                width += scaled.kern(previous, id);
            }
            width += scaled.h_advance(id);
            previous = Some(id);
        }
        width
    }

    fn text(
        &self,
        surface: &mut Surface<'_>,
        text: &str,
        x: f32,
        y: f32,
        size: f32,
        color: Rgb565,
    ) {
        let scale = PxScale::from(size);
        let scaled = self.font.as_scaled(scale);
        let baseline = y + scaled.ascent();
        let mut cursor = x;
        let mut previous = None;
        for character in text.chars() {
            let id = scaled.glyph_id(character);
            if let Some(previous) = previous {
                cursor += scaled.kern(previous, id);
            }
            let glyph = id.with_scale_and_position(scale, point(cursor, baseline));
            if let Some(outline) = self.font.outline_glyph(glyph) {
                let bounds = outline.px_bounds();
                outline.draw(|gx, gy, coverage| {
                    surface.blend_pixel(
                        bounds.min.x.floor() as i32 + gx as i32,
                        bounds.min.y.floor() as i32 + gy as i32,
                        color,
                        (coverage * 255.0).round() as u8,
                    );
                });
            }
            cursor += scaled.h_advance(id);
            previous = Some(id);
        }
    }
}
