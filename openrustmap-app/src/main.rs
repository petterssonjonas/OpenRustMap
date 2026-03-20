mod terminal_restore;

use anyhow::Context;
use crossterm::event::{self, Event, KeyCode, MouseButton, MouseEventKind};
use crossterm::event::EnableMouseCapture;
use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
use crossterm::ExecutableCommand;
use directories::BaseDirs;
use openrustmap_core::{MapInput, MapWidget, OpenRustMapConfig, OpenRustMapWidget, PluginEvent};
use openrustmap_plugin_radiobrowser::RadioBrowserPopup;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::Terminal;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use terminal_restore::{install_panic_hook, TerminalRestoreGuard};

/// Account for the 1-cell map frame on each side.
const MIN_WIDTH: u16 = 95;
const MIN_HEIGHT: u16 = 35;
/// Coalesce mouse-wheel zoom so each scroll “tick” doesn’t schedule 9 tiles + full raster.
const WHEEL_ZOOM_MIN_INTERVAL: Duration = Duration::from_millis(55);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppSettings {
    radio_map_enabled: bool,
    /// When opening Radio-map, start in external-player mode (e.g. mpv) instead of native.
    #[serde(default)]
    radio_prefer_external_player: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            radio_map_enabled: true,
            radio_prefer_external_player: false,
        }
    }
}

fn app_config_dir() -> Option<PathBuf> {
    BaseDirs::new().map(|b| b.config_dir().join("openrustmap"))
}

fn app_settings_path() -> Option<PathBuf> {
    app_config_dir().map(|d| d.join("settings.toml"))
}

#[allow(dead_code)] // reserved for dynamic plugin loading (see ROADMAP)
fn plugins_dir() -> Option<PathBuf> {
    app_config_dir().map(|d| d.join("plugins"))
}

fn radio_plugin_installed() -> bool {
    // Current build ships with the Radio-map plugin compiled in.
    // Later we will switch this to full dynamic discovery from plugins dir.
    true
}

fn load_settings(radio_installed: bool) -> AppSettings {
    let mut s = AppSettings {
        radio_map_enabled: radio_installed,
        radio_prefer_external_player: false,
    };
    if let Some(path) = app_settings_path() {
        if let Ok(raw) = fs::read_to_string(path) {
            if let Ok(parsed) = toml::from_str::<AppSettings>(&raw) {
                s = parsed;
            }
        }
    }
    if !radio_installed {
        s.radio_map_enabled = false;
    }
    s
}

fn save_settings(s: &AppSettings) {
    let Some(path) = app_settings_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(raw) = toml::to_string_pretty(s) {
        let _ = fs::write(path, raw);
    }
}

fn centered_popup(area: Rect, width_pct: u16, height_pct: u16) -> Rect {
    let popup_width = (area.width as u32 * width_pct as u32 / 100) as u16;
    let popup_height = (area.height as u32 * height_pct as u32 / 100) as u16;
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(popup_height)) / 2;
    Rect {
        x,
        y,
        width: popup_width.min(area.width),
        height: popup_height.min(area.height),
    }
}

/// Single-line frame around the map panel (flat map viewport).
fn map_content_rect(map_area: Rect) -> Rect {
    if map_area.width <= 3 || map_area.height <= 3 {
        return map_area;
    }
    Rect {
        x: map_area.x + 1,
        y: map_area.y + 1,
        width: map_area.width.saturating_sub(2),
        height: map_area.height.saturating_sub(2),
    }
}

fn render_map_frame(buf: &mut ratatui::buffer::Buffer, area: Rect) {
    if area.width < 2 || area.height < 2 {
        return;
    }
    let style = Style::default().fg(Color::DarkGray);
    let x0 = area.x;
    let y0 = area.y;
    let x1 = area.x + area.width - 1;
    let y1 = area.y + area.height - 1;
    buf.set_string(x0, y0, "┌", style);
    buf.set_string(x1, y0, "┐", style);
    buf.set_string(x0, y1, "└", style);
    buf.set_string(x1, y1, "┘", style);
    for x in (x0 + 1)..x1 {
        buf.set_string(x, y0, "─", style);
        buf.set_string(x, y1, "─", style);
    }
    for y in (y0 + 1)..y1 {
        buf.set_string(x0, y, "│", style);
        buf.set_string(x1, y, "│", style);
    }
}

fn render_popup_border(buf: &mut ratatui::buffer::Buffer, area: Rect, title: &str) {
    if area.width < 4 || area.height < 3 {
        return;
    }
    let x2 = area.x + area.width - 1;
    let y2 = area.y + area.height - 1;
    for x in area.x..=x2 {
        buf.set_string(x, area.y, "─", Style::default().fg(Color::Gray));
        buf.set_string(x, y2, "─", Style::default().fg(Color::Gray));
    }
    for y in area.y..=y2 {
        buf.set_string(area.x, y, "│", Style::default().fg(Color::Gray));
        buf.set_string(x2, y, "│", Style::default().fg(Color::Gray));
    }
    buf.set_string(area.x, area.y, "┌", Style::default().fg(Color::Gray));
    buf.set_string(x2, area.y, "┐", Style::default().fg(Color::Gray));
    buf.set_string(area.x, y2, "└", Style::default().fg(Color::Gray));
    buf.set_string(x2, y2, "┘", Style::default().fg(Color::Gray));
    let title_text = format!(" {} ", title);
    buf.set_string(area.x + 2, area.y, title_text, Style::default().fg(Color::White));
    let close_x = x2.saturating_sub(2);
    buf.set_string(close_x, area.y, "[X]", Style::default().fg(Color::Red));
}

fn in_rect(col: u16, row: u16, rect: Rect) -> bool {
    col >= rect.x
        && col < rect.x.saturating_add(rect.width)
        && row >= rect.y
        && row < rect.y.saturating_add(rect.height)
}

fn main() -> anyhow::Result<()> {
    install_panic_hook();
    enable_raw_mode()?;
    std::io::stdout().execute(EnterAlternateScreen)?;
    std::io::stdout().execute(EnableMouseCapture)?;
    let _terminal_restore = TerminalRestoreGuard::new();
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend).context("create terminal")?;

    let cfg = OpenRustMapConfig::default();
    let (tile_ready_tx, tile_ready_rx) = mpsc::channel::<()>();
    let mut widget = OpenRustMapWidget::new_with_tile_notify(cfg, Some(tile_ready_tx))
        .context("init map widget")?;
    widget.reset_globe_view();
    let mut radio_popup = RadioBrowserPopup::new();
    let mut settings_popup_visible = false;
    let mut last_station_url: Option<String> = None;
    let radio_installed = radio_plugin_installed();
    let mut app_settings = load_settings(radio_installed);
    save_settings(&app_settings);

    let mut should_quit = false;
    let mut right_dragging = false;
    let mut last_drag: Option<(u16, u16)> = None;
    let mut last_wheel_zoom: Option<Instant> = None;

    while !should_quit {
        // Drain tile completions so the next frame picks up cached bytes.
        let mut had_tile_ready = false;
        while tile_ready_rx.try_recv().is_ok() {
            had_tile_ready = true;
        }
        let mut top_menu_reset = Rect::default();
        let mut top_menu_settings = Rect::default();
        let mut top_menu_radio = Rect::default();
        let mut top_menu_close = Rect::default();
        let mut map_area = Rect::default();
        let mut settings_popup = Rect::default();
        let mut radio_popup_area = Rect::default();
        let mut popup_close_rect = Rect::default();

        terminal.draw(|frame| {
            let area = frame.area();
            let buf = frame.buffer_mut();

            for y in area.y..area.y + area.height {
                for x in area.x..area.x + area.width {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.reset();
                    }
                }
            }

            if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
                let msg = format!(
                    "Terminal too small. Minimum size: {}x{} (current {}x{})",
                    MIN_WIDTH, MIN_HEIGHT, area.width, area.height
                );
                buf.set_string(
                    area.x + 2,
                    area.y + area.height / 2,
                    msg,
                    Style::default().fg(Color::Yellow),
                );
                return;
            }

            // Top menu row.
            let top = Rect { x: area.x, y: area.y, width: area.width, height: 1 };
            let mut cursor = top.x;
            let reset = " Reset ";
            buf.set_string(cursor, top.y, reset, Style::default().fg(Color::Cyan));
            top_menu_reset = Rect { x: cursor, y: top.y, width: reset.len() as u16, height: 1 };
            cursor += reset.len() as u16;

            let settings = " Settings ";
            buf.set_string(cursor, top.y, settings, Style::default().fg(Color::White));
            top_menu_settings = Rect { x: cursor, y: top.y, width: settings.len() as u16, height: 1 };
            cursor += settings.len() as u16;

            if radio_installed {
                let radio = " Radio-map ";
                let style = if app_settings.radio_map_enabled {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                buf.set_string(cursor, top.y, radio, style);
                top_menu_radio = Rect { x: cursor, y: top.y, width: radio.len() as u16, height: 1 };
            }

            let close_label = "[X]";
            let close_x = top.x + top.width.saturating_sub(close_label.len() as u16);
            buf.set_string(close_x, top.y, close_label, Style::default().fg(Color::Red));
            top_menu_close = Rect { x: close_x, y: top.y, width: close_label.len() as u16, height: 1 };

            map_area = Rect {
                x: area.x,
                y: area.y + 1,
                width: area.width,
                height: area.height - 1,
            };

            let map_content = map_content_rect(map_area);
            widget.render(map_content, buf);

            if let Some(url) = &last_station_url {
                let text = format!(" stream: {}", url);
                let y = map_content.y + map_content.height.saturating_sub(1);
                buf.set_string(map_content.x, y, text, Style::default().fg(Color::DarkGray));
            }

            if map_area.width > 3 && map_area.height > 3 {
                render_map_frame(buf, map_area);
            }

            if settings_popup_visible {
                settings_popup = centered_popup(map_area, 52, 45);
                render_popup_border(buf, settings_popup, "Settings");
                let inner_x = settings_popup.x + 2;
                let inner_y = settings_popup.y + 2;
                let status = if app_settings.radio_map_enabled { "enabled" } else { "disabled" };
                buf.set_string(
                    inner_x,
                    inner_y,
                    format!("Radio-map plugin: {}", status),
                    Style::default().fg(Color::White),
                );
                buf.set_string(
                    inner_x,
                    inner_y + 2,
                    "Press 't' — enable Radio-map menu. 'e' — prefer external player (mpv).",
                    Style::default().fg(Color::DarkGray),
                );
                let ext = if app_settings.radio_prefer_external_player {
                    "on"
                } else {
                    "off"
                };
                buf.set_string(
                    inner_x,
                    inner_y + 3,
                    format!("Prefer external player: {ext}"),
                    Style::default().fg(Color::Yellow),
                );
                popup_close_rect = Rect {
                    x: settings_popup.x + settings_popup.width.saturating_sub(3),
                    y: settings_popup.y,
                    width: 3,
                    height: 1,
                };
            }

            if radio_popup.visible {
                radio_popup_area = centered_popup(map_area, 74, 78);
                render_popup_border(buf, radio_popup_area, "Radio-map");
                let inner = Rect {
                    x: radio_popup_area.x + 1,
                    y: radio_popup_area.y + 1,
                    width: radio_popup_area.width.saturating_sub(2),
                    height: radio_popup_area.height.saturating_sub(2),
                };
                radio_popup.render(inner, buf);
                popup_close_rect = Rect {
                    x: radio_popup_area.x + radio_popup_area.width.saturating_sub(3),
                    y: radio_popup_area.y,
                    width: 3,
                    height: 1,
                };
            }
        })?;

        let poll_wait = if had_tile_ready {
            Duration::from_millis(0)
        } else {
            Duration::from_millis(16)
        };

        if event::poll(poll_wait)? {
            let ev = event::read()?;

            match &ev {
                Event::Key(k) if k.code == KeyCode::Char('q') => should_quit = true,
                Event::Key(k) if k.code == KeyCode::Esc => {
                    if radio_popup.visible {
                        radio_popup.close();
                    } else if settings_popup_visible {
                        settings_popup_visible = false;
                    }
                }
                Event::Key(k) if k.code == KeyCode::Char('r') => widget.reset_globe_view(),
                Event::Key(k) if k.code == KeyCode::Char('s') => {
                    settings_popup_visible = !settings_popup_visible;
                }
                Event::Key(k) if settings_popup_visible && k.code == KeyCode::Char('t') => {
                    if radio_installed {
                        app_settings.radio_map_enabled = !app_settings.radio_map_enabled;
                        save_settings(&app_settings);
                    }
                }
                Event::Key(k) if settings_popup_visible && k.code == KeyCode::Char('e') => {
                    if radio_installed {
                        app_settings.radio_prefer_external_player =
                            !app_settings.radio_prefer_external_player;
                        save_settings(&app_settings);
                    }
                }
                Event::Mouse(m) => {
                    // top menu clicks
                    if m.kind == MouseEventKind::Down(MouseButton::Left) {
                        if in_rect(m.column, m.row, top_menu_close) {
                            should_quit = true;
                        } else if in_rect(m.column, m.row, top_menu_reset) {
                            widget.reset_globe_view();
                        } else if in_rect(m.column, m.row, top_menu_settings) {
                            settings_popup_visible = !settings_popup_visible;
                        } else if radio_installed
                            && app_settings.radio_map_enabled
                            && in_rect(m.column, m.row, top_menu_radio)
                        {
                            let (lat, lon) = widget.center();
                            radio_popup.set_map_center(lat, lon);
                            radio_popup.set_prefer_external_player(
                                app_settings.radio_prefer_external_player,
                            );
                            radio_popup.open();
                        } else if in_rect(m.column, m.row, popup_close_rect) {
                            if radio_popup.visible {
                                radio_popup.close();
                            }
                            if settings_popup_visible {
                                settings_popup_visible = false;
                            }
                        }
                    }

                    let block_map_mouse = radio_popup.visible || settings_popup_visible;
                    if !block_map_mouse {
                        match m.kind {
                            MouseEventKind::ScrollUp => {
                                let now = Instant::now();
                                if last_wheel_zoom.map_or(true, |t| {
                                    now.duration_since(t) >= WHEEL_ZOOM_MIN_INTERVAL
                                }) {
                                    last_wheel_zoom = Some(now);
                                    widget.zoom_by(0.3);
                                }
                            }
                            MouseEventKind::ScrollDown => {
                                let now = Instant::now();
                                if last_wheel_zoom.map_or(true, |t| {
                                    now.duration_since(t) >= WHEEL_ZOOM_MIN_INTERVAL
                                }) {
                                    last_wheel_zoom = Some(now);
                                    widget.zoom_by(-0.3);
                                }
                            }
                            MouseEventKind::Down(MouseButton::Right) => {
                                right_dragging = true;
                                last_drag = Some((m.column, m.row));
                            }
                            MouseEventKind::Drag(MouseButton::Right) => {
                                if right_dragging {
                                    if let Some((px, py)) = last_drag {
                                        let dx = m.column as i16 - px as i16;
                                        let dy = m.row as i16 - py as i16;
                                        // Match “grab the map”: drag direction = map motion (was inverted).
                                        // Flat map: horizontal follows pointer; vertical inverted vs raw deltas.
                                        widget.pan_by_cells(dx, -dy);
                                    }
                                    last_drag = Some((m.column, m.row));
                                }
                            }
                            MouseEventKind::Up(MouseButton::Right) => {
                                right_dragging = false;
                                last_drag = None;
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }

            if radio_popup.visible {
                radio_popup.handle_input(MapInput::CEvent(ev));
                if let Some(PluginEvent::StationSelected(sel)) = radio_popup.take_event() {
                    last_station_url = Some(sel.station.stream_url.clone());
                    if let (Some(lat), Some(lon)) = (sel.station.geo_lat, sel.station.geo_long) {
                        widget.set_center(lat, lon);
                    }
                }
            } else if !settings_popup_visible {
                widget.handle_input(MapInput::CEvent(ev));
            }
        }
    }

    let _ = terminal.show_cursor();
    Ok(())
}
