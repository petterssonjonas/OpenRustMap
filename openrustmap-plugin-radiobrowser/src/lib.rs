//! Radio-Browser integration plugin for OpenRustMap.
//!
//! v1 implementation will include an in-map popup to search/pick stations.

mod player;

use openrustmap_core::plugin::{
    PluginEvent, RadioPlayMode, StationInfo, StationSelected, StationSelectedAction,
};
use openrustmap_core::widget::MapInput;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};
use serde::Deserialize;

use crossbeam_channel::Sender;
use player::RadioPlayer;
use std::collections::HashMap;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct RadioBrowserPlugin {
    pub play_mode: RadioPlayMode,
    pending_event: Option<PluginEvent>,
}

impl Default for RadioBrowserPlugin {
    fn default() -> Self {
        Self {
            play_mode: RadioPlayMode::PlayInPlugin,
            pending_event: None,
        }
    }
}

impl RadioBrowserPlugin {
    /// Called by the plugin UI when the user selects a station.
    pub fn station_picked(&mut self, station: StationInfo) {
        let action = match self.play_mode {
            RadioPlayMode::PlayInPlugin => StationSelectedAction::PlayInPlugin,
            RadioPlayMode::UrlOnly => StationSelectedAction::UrlOnly,
            RadioPlayMode::ExternalPlayer => StationSelectedAction::ExternalPlayer,
        };

        self.pending_event = Some(PluginEvent::StationSelected(StationSelected {
            station,
            action,
        }));
    }

    pub fn take_event(&mut self) -> Option<PluginEvent> {
        self.pending_event.take()
    }
}

/// Standalone popup UI for Radio-Browser station search + picking.
///
/// For v1 we keep this simple and synchronous:
/// - typing a station name performs a search (on `Enter`)
/// - arrow keys move selection
/// - `Enter` picks the station (emits `PluginEvent::StationSelected`)
#[derive(Debug)]
pub struct RadioBrowserPopup {
    pub visible: bool,
    pub play_mode: RadioPlayMode,
    query: String,
    results: Vec<StationInfo>,
    nearby_results: Vec<StationInfo>,
    selected: usize,
    pending_event: Option<PluginEvent>,
    event_tx: Option<Sender<PluginEvent>>,
    status: Option<String>,
    query_dirty: bool,
    in_search_mode: bool,
    map_center_lat: f64,
    map_center_lon: f64,
    now_playing_url: Option<String>,

    last_search_at: Option<Instant>,
    result_cache: HashMap<String, Vec<StationInfo>>,

    // Best-effort: if you want full server failover, we’ll implement it in v2.
    api_base_url: String,
    http: reqwest::blocking::Client,
    player: RadioPlayer,

    /// Last `render` content area (for mouse hit-testing).
    last_content_rect: Rect,
    /// Executable for `RadioPlayMode::ExternalPlayer` (default `mpv`).
    external_player_exe: String,
    external_child: Option<Child>,
}

impl Default for RadioBrowserPopup {
    fn default() -> Self {
        Self::new()
    }
}

impl RadioBrowserPopup {
    pub fn new() -> Self {
        Self {
            visible: false,
            play_mode: RadioPlayMode::PlayInPlugin,
            query: String::new(),
            results: Vec::new(),
            nearby_results: Vec::new(),
            selected: 0,
            pending_event: None,
            status: None,
            event_tx: None,
            query_dirty: true,
            in_search_mode: false,
            map_center_lat: 20.0,
            map_center_lon: 0.0,
            now_playing_url: None,
            last_search_at: None,
            result_cache: HashMap::new(),
            api_base_url: "https://api.radio-browser.info".to_string(),
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .unwrap(),
            player: RadioPlayer::new(),
            last_content_rect: Rect::default(),
            external_player_exe: std::env::var("OPENRUSTMAP_RADIO_PLAYER")
                .unwrap_or_else(|_| "mpv".into()),
            external_child: None,
        }
    }

    pub fn open(&mut self) {
        self.visible = true;
        self.query.clear();
        self.results.clear();
        self.selected = 0;
        self.pending_event = None;
        self.status = Some("Type a station name, press Enter to search.".into());
        self.query_dirty = true;
        self.last_search_at = None;
        self.in_search_mode = false;
        let _ = self.fetch_nearby_now();
    }

    pub fn close(&mut self) {
        self.visible = false;
        self.pending_event = None;
        self.kill_external_player();
    }

    pub fn take_event(&mut self) -> Option<PluginEvent> {
        self.pending_event.take()
    }

    /// Optional host callback. If set, selecting a station sends the plugin
    /// event immediately instead of requiring polling via `take_event()`.
    pub fn set_event_tx(&mut self, tx: Sender<PluginEvent>) {
        self.event_tx = Some(tx);
    }

    pub fn set_map_center(&mut self, lat: f64, lon: f64) {
        self.map_center_lat = lat;
        self.map_center_lon = lon;
    }

    fn action_for_play_mode(&self) -> StationSelectedAction {
        match self.play_mode {
            RadioPlayMode::PlayInPlugin => StationSelectedAction::PlayInPlugin,
            RadioPlayMode::UrlOnly => StationSelectedAction::UrlOnly,
            RadioPlayMode::ExternalPlayer => StationSelectedAction::ExternalPlayer,
        }
    }

    /// If true, next time the popup is shown, start in external-player mode (`mpv`).
    /// If false, any `m`-key mode choice is kept between sessions in memory.
    pub fn set_prefer_external_player(&mut self, prefer: bool) {
        if prefer {
            self.play_mode = RadioPlayMode::ExternalPlayer;
        }
    }

    fn poll_player_errors(&mut self) {
        if let Some(err) = self.player.state.take_last_error() {
            self.status = Some(format!("Playback: {err}"));
        }
    }

    fn kill_external_player(&mut self) {
        if let Some(mut ch) = self.external_child.take() {
            let _ = ch.kill();
            let _ = ch.wait();
        }
    }

    fn spawn_external(&mut self, url: &str) -> anyhow::Result<()> {
        self.kill_external_player();
        let child = Command::new(&self.external_player_exe)
            .args(["--no-video", "--really-quiet", url])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("{}: {e}", self.external_player_exe))?;
        self.external_child = Some(child);
        Ok(())
    }

    pub fn handle_input(&mut self, input: MapInput) {
        let ev = match input {
            MapInput::CEvent(ev) => ev,
            MapInput::Tick => {
                if self.visible {
                    self.poll_player_errors();
                }
                return;
            }
        };
        if !self.visible {
            return;
        }
        self.poll_player_errors();

        if let Event::Mouse(m) = &ev {
            self.handle_mouse(m);
            return;
        }

        let Event::Key(KeyEvent { code, modifiers, .. }) = ev else {
            return;
        };

        if modifiers.contains(KeyModifiers::CONTROL) {
            return;
        }

        match code {
            KeyCode::Esc => self.close(),
            KeyCode::Char('x') => {
                self.player.stop();
                self.kill_external_player();
                self.status = Some("Playback stopped.".into());
                self.now_playing_url = None;
            }
            KeyCode::Char('p') => {
                if matches!(self.play_mode, RadioPlayMode::PlayInPlugin) {
                    self.player.toggle_pause();
                    let msg = if self.player.state.is_paused() {
                        "Paused (native)."
                    } else {
                        "Resumed (native)."
                    };
                    self.status = Some(msg.into());
                } else {
                    self.status = Some("Pause only applies to native (in-plugin) playback.".into());
                }
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                if matches!(self.play_mode, RadioPlayMode::PlayInPlugin) {
                    let v = (self.player.state.volume() + 0.08).min(1.0);
                    self.player.set_volume_command(v);
                    self.status = Some(format!("Volume: {:.0}%", v * 100.0));
                }
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                if matches!(self.play_mode, RadioPlayMode::PlayInPlugin) {
                    let v = (self.player.state.volume() - 0.08).max(0.0);
                    self.player.set_volume_command(v);
                    self.status = Some(format!("Volume: {:.0}%", v * 100.0));
                }
            }
            KeyCode::Tab => {
                self.in_search_mode = !self.in_search_mode;
                self.selected = 0;
                if !self.in_search_mode {
                    let _ = self.fetch_nearby_now();
                }
            }
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
            }
            KeyCode::Down => {
                let len = if self.in_search_mode {
                    self.results.len()
                } else {
                    self.nearby_results.len()
                };
                if self.selected + 1 < len {
                    self.selected += 1;
                }
            }
            KeyCode::Char('m') => {
                self.play_mode = match self.play_mode {
                    RadioPlayMode::PlayInPlugin => RadioPlayMode::UrlOnly,
                    RadioPlayMode::UrlOnly => RadioPlayMode::ExternalPlayer,
                    RadioPlayMode::ExternalPlayer => RadioPlayMode::PlayInPlugin,
                };
                self.status = Some(format!("Mode: {:?}", self.play_mode));
            }
            KeyCode::Enter => {
                if self.query.trim().is_empty() {
                    if !self.in_search_mode {
                        // Nearby list selection with Enter.
                        if let Some(station) = self.nearby_results.get(self.selected).cloned() {
                            self.emit_station(station);
                        }
                        return;
                    }
                    return;
                }
                if self.query_dirty || !self.in_search_mode {
                    // Lightweight rate limiting for v1.
                    if let Some(t) = self.last_search_at {
                        if t.elapsed() < Duration::from_millis(900) {
                            self.status =
                                Some("Please wait a moment before searching again.".into());
                            return;
                        }
                    }
                    self.last_search_at = Some(Instant::now());
                    self.status = Some(if self.in_search_mode {
                        "Searching…".into()
                    } else {
                        "Loading nearby stations…".into()
                    });
                    let res = if self.in_search_mode {
                        self.search_now()
                    } else {
                        self.fetch_nearby_now()
                    };
                    if let Err(e) = res {
                        self.status = Some(format!("Search failed: {e}"));
                    } else {
                        self.status = None;
                        self.selected = 0;
                    }
                } else {
                    // Query already searched; Enter selects the highlighted result.
                    if self.in_search_mode {
                        if let Some(station) = self.results.get(self.selected).cloned() {
                            self.emit_station(station);
                        }
                    } else if let Some(station) = self.nearby_results.get(self.selected).cloned() {
                        self.emit_station(station);
                    }
                }
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.query_dirty = true;
            }
            KeyCode::Char(ch) => {
                self.in_search_mode = true;
                self.query.push(ch);
                self.query_dirty = true;
            }
            _ => {}
        }
    }

    fn search_now(&mut self) -> anyhow::Result<()> {
        #[derive(Debug, Deserialize)]
        struct StationResponse {
            stationuuid: String,
            name: String,
            url_resolved: String,
            url: String,
            geo_lat: Option<f64>,
            geo_long: Option<f64>,
        }

        // Cache lookup by query string.
        if let Some(cached) = self.result_cache.get(&self.query) {
            self.results = cached.clone();
            self.query_dirty = false;
            self.status = None;
            return Ok(());
        }

        // Radio-Browser parameter names are snake_case.
        // `has_geo_info=true` filters for stations with coordinates.
        let url = format!(
            "{base}/json/stations/search?name={query}&limit=15&hidebroken=true&has_geo_info=true",
            base = self.api_base_url,
            query = urlencoding::encode(&self.query),
        );

        let resp = self.http.get(url).send()?;
        if !resp.status().is_success() {
            anyhow::bail!("HTTP {}", resp.status());
        }
        let stations: Vec<StationResponse> = resp.json()?;

        self.results = stations
            .into_iter()
            .map(|s| StationInfo {
                station_uuid: s.stationuuid,
                name: s.name,
                stream_url: if s.url_resolved.trim().is_empty() {
                    s.url
                } else {
                    s.url_resolved
                },
                geo_lat: s.geo_lat,
                geo_long: s.geo_long,
            })
            .collect();

        // Best-effort cache update.
        if self.result_cache.len() > 10 {
            self.result_cache.clear();
        }
        self.result_cache.insert(self.query.clone(), self.results.clone());

        self.query_dirty = false;
        self.in_search_mode = true;

        Ok(())
    }

    fn fetch_nearby_now(&mut self) -> anyhow::Result<()> {
        #[derive(Debug, Deserialize)]
        struct StationResponse {
            stationuuid: String,
            name: String,
            url_resolved: String,
            url: String,
            geo_lat: Option<f64>,
            geo_long: Option<f64>,
        }

        let url = format!(
            "{base}/json/stations/bygeo?lat={lat}&lng={lon}&limit=20&hidebroken=true",
            base = self.api_base_url,
            lat = self.map_center_lat,
            lon = self.map_center_lon,
        );
        let resp = self.http.get(url).send()?;
        if !resp.status().is_success() {
            anyhow::bail!("HTTP {}", resp.status());
        }
        let stations: Vec<StationResponse> = resp.json()?;
        self.nearby_results = stations
            .into_iter()
            .map(|s| StationInfo {
                station_uuid: s.stationuuid,
                name: s.name,
                stream_url: if s.url_resolved.trim().is_empty() { s.url } else { s.url_resolved },
                geo_lat: s.geo_lat,
                geo_long: s.geo_long,
            })
            .collect();
        self.in_search_mode = false;
        self.query_dirty = false;
        Ok(())
    }

    fn emit_station(&mut self, station: StationInfo) {
        let selected_url = station.stream_url.clone();
        let has_geo = station.geo_lat.is_some() && station.geo_long.is_some();
        let evt = PluginEvent::StationSelected(StationSelected {
            station,
            action: self.action_for_play_mode(),
        });
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(evt);
        } else {
            self.pending_event = Some(evt);
        }
        match self.play_mode {
            RadioPlayMode::PlayInPlugin => {
                self.player.play(selected_url.clone());
                self.now_playing_url = Some(selected_url);
                self.status = Some("Playing stream (native). p: pause, +/- volume.".into());
            }
            RadioPlayMode::ExternalPlayer => {
                self.player.stop();
                match self.spawn_external(&selected_url) {
                    Ok(()) => {
                        self.now_playing_url = Some(selected_url);
                        self.status = Some(format!(
                            "Playing in {} (external).",
                            self.external_player_exe
                        ));
                    }
                    Err(e) => {
                        self.status = Some(format!("External player: {e}"));
                        self.now_playing_url = Some(selected_url.clone());
                    }
                }
            }
            RadioPlayMode::UrlOnly => {
                self.now_playing_url = Some(selected_url);
                self.status = Some("URL only — stream not started.".into());
            }
        }
        if has_geo {
            self.visible = false;
        } else if let Some(prev) = self.status.clone() {
            self.status = Some(format!("{prev} (no station geo — map not centered)"));
        } else {
            self.status = Some("No station geo — map not centered.".into());
        }
    }

    fn handle_mouse(&mut self, m: &MouseEvent) {
        let area = self.last_content_rect;
        if area.width == 0 || area.height == 0 {
            return;
        }

        match m.kind {
            MouseEventKind::ScrollUp => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
            }
            MouseEventKind::ScrollDown => {
                let len = if self.in_search_mode {
                    self.results.len()
                } else {
                    self.nearby_results.len()
                };
                if self.selected + 1 < len {
                    self.selected += 1;
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let list_start_y = area.y + 5;
                if m.column >= area.x
                    && m.column < area.x + area.width
                    && m.row >= list_start_y
                    && m.row < area.y + area.height
                {
                    let line = (m.row - list_start_y) as usize;
                    let start_idx = self.selected.saturating_sub(5);
                    let idx = start_idx + line;
                    let len = if self.in_search_mode {
                        self.results.len()
                    } else {
                        self.nearby_results.len()
                    };
                    if idx < len {
                        self.selected = idx;
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                let list_start_y = area.y + 5;
                if m.column >= area.x
                    && m.column < area.x + area.width
                    && m.row >= list_start_y
                    && m.row < area.y + area.height
                {
                    let line = (m.row - list_start_y) as usize;
                    let start_idx = self.selected.saturating_sub(5);
                    let idx = start_idx + line;
                    let len = if self.in_search_mode {
                        self.results.len()
                    } else {
                        self.nearby_results.len()
                    };
                    if idx < len {
                        self.selected = idx;
                        let station = if self.in_search_mode {
                            self.results.get(idx).cloned()
                        } else {
                            self.nearby_results.get(idx).cloned()
                        };
                        if let Some(st) = station {
                            self.emit_station(st);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    pub fn render(&mut self, area: Rect, buf: &mut Buffer) {
        if !self.visible {
            return;
        }

        self.last_content_rect = area;
        self.poll_player_errors();

        // Clear area.
        for y in area.y..area.y + area.height {
            for x in area.x..area.x + area.width {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.reset();
                }
            }
        }

        let title = "Radio Browser";
        let mode = match self.play_mode {
            RadioPlayMode::PlayInPlugin => "PlayInPlugin",
            RadioPlayMode::UrlOnly => "UrlOnly",
            RadioPlayMode::ExternalPlayer => "ExternalPlayer",
        };

        let lines: Vec<String> = {
            let mut v = Vec::new();
            v.push(format!("{title}  (m mode · Tab · Esc · mouse: wheel scroll, R-click play)"));
            v.push(format!("Mode: {}", if self.in_search_mode { "Search" } else { "Nearby (20)" }));
            v.push(format!("Query: {}", self.query));
            v.push(format!("Playback: {mode}  (env OPENRUSTMAP_RADIO_PLAYER, default mpv)"));
            if let Some(status) = &self.status {
                v.push(status.clone());
            } else {
                v.push("Enter pick · ↑↓ · x stop · p pause(native) · +/- vol".into());
            }
            if let Some(url) = &self.now_playing_url {
                v.push(format!("Now playing: {}", url));
            }
            v
        };

        for (i, line) in lines.iter().take(area.height as usize) .enumerate() {
            buf.set_string(area.x, area.y + i as u16, line, Style::default().fg(Color::White));
        }

        // Results list.
        let list_start_y = area.y + 5;
        let max_lines = (area.y + area.height).saturating_sub(list_start_y) as usize;
        let start_idx = self.selected.saturating_sub(5);
        let active = if self.in_search_mode { &self.results } else { &self.nearby_results };

        for i in 0..max_lines {
            let idx = start_idx + i;
            if idx >= active.len() {
                break;
            }
            let station = &active[idx];
            let prefix = if idx == self.selected { ">" } else { " " };
            let left = format!("{prefix} {}", station.name);
            let y = list_start_y + i as u16;
            let color = if idx == self.selected {
                Color::Yellow
            } else {
                Color::DarkGray
            };
            buf.set_string(area.x, y, left, Style::default().fg(color));
        }
    }
}

/// Quick helper to keep popup code compiling without pulling extra deps in v1.
///
/// We'll likely replace this with a proper text wrapping widget later.
fn _format_safely(s: &str) -> &str {
    s
}
