use crate::config::OpenRustMapConfig;
use crate::tile_source::TileSource;
use crate::widget::{MapInput, MapWidget};
use anyhow::Context;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use open_vector_tile::{VectorFeatureMethods, VectorLayerMethods, VectorTile};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};
use std::sync::Arc;
use std::sync::mpsc::Sender;

const TILE_RANGE: u32 = 14;
const PROJECT_SIZE: f64 = 256.0;

/// Maximum view zoom. Vector tiles are only fetched up to [`TILE_RANGE`] (z14); values above
/// that **magnify** the z14 tile (more detail on screen, same PBFs).
pub const MAX_VIEW_ZOOM: f64 = 16.0;

/// Cap raster work per frame; tightened automatically at high zoom where tiles are dense.
fn segment_budget_for_zoom(base_z: u32) -> usize {
    match base_z {
        0..=5 => 38_000,
        6..=8 => 22_000,
        9..=10 => 12_000,
        11..=12 => 8_000,
        _ => 5_000,
    }
}

/// Fewer layers at high zoom: buildings/labels dominate segment count.
fn candidate_layers_for_zoom(base_z: u32) -> &'static [&'static str] {
    const FULL: &[&str] = &[
        "road",
        "water",
        "landuse",
        "building",
        "admin",
        "place_label",
        "poi_label",
        "rail_station_label",
    ];
    const NO_LABELS: &[&str] = &["road", "water", "landuse", "building", "admin"];
    const MID: &[&str] = &["road", "water", "landuse", "admin"];
    /// z11+: drop landuse/admin — keep major linework only (still shows context at z14).
    const LIGHT: &[&str] = &["road", "water"];

    match base_z {
        0..=5 => FULL,
        6..=8 => NO_LABELS,
        9..=10 => MID,
        _ => LIGHT,
    }
}

fn base_zoom(zoom: f64) -> u32 {
    let z = zoom.floor();
    let z = z.max(0.0).min(TILE_RANGE as f64);
    z as u32
}

fn tile_size_at_zoom(zoom: f64) -> f64 {
    let z0 = base_zoom(zoom) as i64;
    let dz = zoom - z0 as f64;
    PROJECT_SIZE * 2.0_f64.powf(dz)
}

fn ll2tile(lon: f64, lat: f64, zoom: u32) -> (f64, f64) {
    let n = 2.0_f64.powi(zoom as i32);
    let x = (lon + 180.0) / 360.0 * n;
    let lat_rad = lat.to_radians();
    let y = (1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n;
    (x, y)
}

fn mod_wrap(n: i64, m: i64) -> i64 {
    let mut v = n % m;
    if v < 0 {
        v += m;
    }
    v
}

/// Braille dot mapping (same as MapSCII).
fn pixel_mask_for(px: i32, py: i32) -> u8 {
    let bx = (px & 1) as i32;
    let by = (py & 3) as i32;
    match (bx, by) {
        (0, 0) => 0x01,
        (1, 0) => 0x08,
        (0, 1) => 0x02,
        (1, 1) => 0x10,
        (0, 2) => 0x04,
        (1, 2) => 0x20,
        (0, 3) => 0x40,
        (1, 3) => 0x80,
        _ => 0,
    }
}

struct BrailleFrame {
    width_px: u32,
    height_px: u32,
    width_cells: u32,
    height_cells: u32,
    /// mask per cell (0..255)
    masks: Vec<u8>,
}

impl BrailleFrame {
    fn new(width_cells: u32, height_cells: u32) -> Self {
        let width_px = width_cells * 2;
        let height_px = height_cells * 4;
        Self {
            width_px,
            height_px,
            width_cells,
            height_cells,
            masks: vec![0; (width_cells * height_cells) as usize],
        }
    }

    fn set_pixel(&mut self, x: i32, y: i32) {
        if x < 0 || y < 0 {
            return;
        }
        if x >= self.width_px as i32 || y >= self.height_px as i32 {
            return;
        }

        let cell_x = (x >> 1) as u32;
        let cell_y = (y >> 2) as u32;
        let idx = (cell_x + self.width_cells * cell_y) as usize;
        let mask = pixel_mask_for(x, y);
        self.masks[idx] |= mask;
    }

    fn cell_char(mask: u8) -> char {
        // U+2800 + 8-dot pattern.
        std::char::from_u32(0x2800u32 + mask as u32).unwrap_or(' ')
    }
}

fn draw_line(frame: &mut BrailleFrame, x0: i32, y0: i32, x1: i32, y1: i32) {
    let mut x = x0;
    let mut y = y0;

    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;

    loop {
        frame.set_pixel(x, y);
        if x == x1 && y == y1 {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}

#[derive(Debug, Clone)]
pub struct MapViewState {
    pub center_lat: f64,
    pub center_lon: f64,
    pub zoom: f64,
    pub braille: bool,
}

pub struct OpenRustMapWidget {
    pub config: OpenRustMapConfig,
    pub state: MapViewState,
    tile_source: Arc<TileSource>,
    /// Cache which tile keys we attempted in the last render to avoid repeated logging.
    last_render_tiles: Vec<String>,
}

impl OpenRustMapWidget {
    pub fn new(config: OpenRustMapConfig) -> anyhow::Result<Self> {
        Self::new_with_tile_notify(config, None)
    }

    /// Optional `notify` is signaled when a background tile fetch completes (wake UI redraw).
    pub fn new_with_tile_notify(
        config: OpenRustMapConfig,
        notify: Option<Sender<()>>,
    ) -> anyhow::Result<Self> {
        let tile_source = Arc::new(
            TileSource::with_notify(&config.tile_source, notify).context("init tile source")?,
        );

        Ok(Self {
            config: config.clone(),
            state: MapViewState {
                center_lat: config.initial_center_lat,
                center_lon: config.initial_center_lon,
                zoom: config.initial_zoom.clamp(0.0, MAX_VIEW_ZOOM),
                braille: config.braille,
            },
            tile_source,
            last_render_tiles: Vec::new(),
        })
    }

    fn normalize_center(&mut self) {
        // Clamp lat to web mercator-ish range.
        let mut lat = self.state.center_lat;
        if lat > 85.0511 {
            lat = 85.0511;
        } else if lat < -85.0511 {
            lat = -85.0511;
        }

        let mut lon = self.state.center_lon;
        if lon < -180.0 || lon > 180.0 {
            lon = ((lon + 180.0).rem_euclid(360.0)) - 180.0;
        }

        self.state.center_lat = lat;
        self.state.center_lon = lon;
        self.state.zoom = self.state.zoom.clamp(0.0, MAX_VIEW_ZOOM);
    }

    pub fn center(&self) -> (f64, f64) {
        (self.state.center_lat, self.state.center_lon)
    }

    pub fn zoom(&self) -> f64 {
        self.state.zoom
    }

    pub fn reset_globe_view(&mut self) {
        // Slightly above Greenwich; zoomed-out world view.
        self.state.center_lat = 20.0;
        self.state.center_lon = 0.0;
        self.state.zoom = 0.0;
    }

    pub fn zoom_by(&mut self, delta: f64) {
        self.state.zoom = (self.state.zoom + delta).clamp(0.0, MAX_VIEW_ZOOM);
    }

    pub fn pan_by_degrees(&mut self, dlat: f64, dlon: f64) {
        self.state.center_lat += dlat;
        self.state.center_lon += dlon;
        self.normalize_center();
    }

    pub fn pan_by_cells(&mut self, dx_cells: i16, dy_cells: i16) {
        // Convert cell movement to a rough lat/lon delta scaled by zoom.
        let lon_step = 4.0 / 2.0_f64.powf(self.state.zoom.max(0.0));
        let lat_step = 3.0 / 2.0_f64.powf(self.state.zoom.max(0.0));
        self.pan_by_degrees(-(dy_cells as f64) * lat_step, -(dx_cells as f64) * lon_step);
    }

    fn render_frame(
        &mut self,
        frame: &mut BrailleFrame,
        view_width_px: u32,
        view_height_px: u32,
    ) {
        let base_z = base_zoom(self.state.zoom);
        let segment_budget = segment_budget_for_zoom(base_z);
        let candidate_layers = candidate_layers_for_zoom(base_z);
        let (center_tx, center_ty) = ll2tile(self.state.center_lon, self.state.center_lat, base_z);
        let tile_size = tile_size_at_zoom(self.state.zoom);

        let grid_size = 2_i64.pow(base_z);

        let center_tile_x = center_tx.floor() as i64;
        let center_tile_y = center_ty.floor() as i64;

        let tile_x_min = center_tile_x - 1;
        let tile_x_max = center_tile_x + 1;
        let tile_y_min = center_tile_y - 1;
        let tile_y_max = center_tile_y + 1;

        // Clear masks.
        frame.masks.fill(0);

        let mut tiles_seen = Vec::new();
        let mut segments_drawn = 0usize;

        for ty in tile_y_min..=tile_y_max {
            for tx in tile_x_min..=tile_x_max {
                // Wrap X like slippy maps.
                let wrapped_x = mod_wrap(tx, grid_size) as u32;
                let y = ty as i32;
                if y < 0 {
                    continue;
                }

                // Basic tile bounds for Y (no wrap).
                let max_y = grid_size as i32 - 1;
                if y > max_y {
                    continue;
                }

                let tile_key = format!("z{}-x{}-y{}", base_z, wrapped_x, y);
                tiles_seen.push(tile_key.clone());

                let tile_bytes = match self
                    .tile_source
                    .try_get_tile_bytes(base_z, wrapped_x, y as u32)
                {
                    Some(b) => b,
                    None => {
                        self.tile_source
                            .request_tile(base_z, wrapped_x, y as u32);
                        continue;
                    }
                };

                // Parse MVT.
                let mut vt = VectorTile::new(tile_bytes, None);
                vt.read_layers();

                for layer_name in candidate_layers {
                    let Some(layer) = vt.layer(layer_name) else {
                        continue;
                    };

                    let extent = layer.extent() as f64;
                    let len = layer.len();
                    for i in 0..len {
                        let Some(mut feature) = layer.feature(i) else {
                            continue;
                        };

                        // Lines and polygon outlines (v1).
                        if feature.is_lines() {
                            let lines = feature.load_lines();
                            for line in lines {
                                if line.geometry.len() < 2 {
                                    continue;
                                }
                                for j in 0..(line.geometry.len() - 1) {
                                    if segments_drawn >= segment_budget {
                                        self.last_render_tiles = tiles_seen;
                                        return;
                                    }
                                    let p0 = &line.geometry[j];
                                    let p1 = &line.geometry[j + 1];

                                    // Convert tile-coordinates -> framebuffer pixels.
                                    // MapSCII conceptually positions tiles around the center.
                                    let tile_screen_x = (view_width_px as f64 / 2.0)
                                        - (center_tx - tx as f64) * tile_size;
                                    let tile_screen_y = (view_height_px as f64 / 2.0)
                                        - (center_ty - ty as f64) * tile_size;

                                    let x0 = (tile_screen_x + (p0.x as f64 / extent) * tile_size)
                                        .round() as i32;
                                    let y0 = (tile_screen_y + (p0.y as f64 / extent) * tile_size)
                                        .round() as i32;
                                    let x1 = (tile_screen_x + (p1.x as f64 / extent) * tile_size)
                                        .round() as i32;
                                    let y1 = (tile_screen_y + (p1.y as f64 / extent) * tile_size)
                                        .round() as i32;

                                    draw_line(frame, x0, y0, x1, y1);
                                    segments_drawn += 1;
                                }
                            }
                        } else if feature.is_polygons() {
                            let polys = feature.load_polys();
                            for poly in polys {
                                for ring in poly {
                                    let pts = &ring.geometry;
                                    if pts.len() < 2 {
                                        continue;
                                    }

                                    // Draw ring segments.
                                    for j in 0..pts.len() {
                                        if segments_drawn >= segment_budget {
                                            self.last_render_tiles = tiles_seen;
                                            return;
                                        }
                                        let p0 = &pts[j];
                                        let p1 = if j + 1 < pts.len() {
                                            &pts[j + 1]
                                        } else {
                                            // close ring (best-effort)
                                            &pts[0]
                                        };

                                        // Convert tile-coordinates -> framebuffer pixels.
                                        let tile_screen_x = (view_width_px as f64 / 2.0)
                                            - (center_tx - tx as f64) * tile_size;
                                        let tile_screen_y = (view_height_px as f64 / 2.0)
                                            - (center_ty - ty as f64) * tile_size;

                                        let x0 = (tile_screen_x + (p0.x as f64 / extent) * tile_size)
                                            .round() as i32;
                                        let y0 = (tile_screen_y + (p0.y as f64 / extent) * tile_size)
                                            .round() as i32;
                                        let x1 = (tile_screen_x + (p1.x as f64 / extent) * tile_size)
                                            .round() as i32;
                                        let y1 = (tile_screen_y + (p1.y as f64 / extent) * tile_size)
                                            .round() as i32;

                                        draw_line(frame, x0, y0, x1, y1);
                                        segments_drawn += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        self.last_render_tiles = tiles_seen;
    }
}

impl MapWidget for OpenRustMapWidget {
    fn render(&mut self, area: Rect, buf: &mut Buffer) {
        if area.width < 5 || area.height < 5 {
            return;
        }

        self.normalize_center();

        let width_cells: u32 = area.width.into();
        let height_cells: u32 = area.height.into();
        let mut frame = BrailleFrame::new(width_cells, height_cells);
        self.render_frame(&mut frame, width_cells * 2, height_cells * 4);

        // Draw into ratatui buffer.
        let style = Style::default().fg(Color::Cyan);
        for cy in 0..height_cells {
            for cx in 0..width_cells {
                let idx = (cx + width_cells * cy) as usize;
                let mask = frame.masks[idx];
                let ch = if mask == 0 { ' ' } else { BrailleFrame::cell_char(mask) };
                let ch_str = ch.to_string();
                if let Some(cell) = buf.cell_mut((area.x + cx as u16, area.y + cy as u16)) {
                    cell.set_symbol(&ch_str);
                    cell.set_style(style);
                }
            }
        }

        // Also show a footer line style hint in the corner (subtle).
        let footer = format!("lat {:.2} lon {:.2} z {:.2}", self.state.center_lat, self.state.center_lon, self.state.zoom);
        let _ = footer;
    }

    fn handle_input(&mut self, input: MapInput) {
        match input {
            MapInput::CEvent(Event::Key(KeyEvent { code, modifiers, .. })) => {
                // Ctrl+Q-like emergency not required in widget; host handles.
                if modifiers.contains(KeyModifiers::CONTROL) {
                    return;
                }
                match code {
                    KeyCode::Char('c') => self.state.braille = !self.state.braille,
                    KeyCode::Char('a') => {
                        self.state.zoom = (self.state.zoom + 0.2).min(MAX_VIEW_ZOOM);
                    }
                    KeyCode::Char('z') => {
                        self.state.zoom = (self.state.zoom - 0.2).max(0.0);
                    }
                    KeyCode::Left => {
                        let step = 8.0 / 2.0_f64.powf(self.state.zoom);
                        self.state.center_lon -= step;
                    }
                    KeyCode::Right => {
                        let step = 8.0 / 2.0_f64.powf(self.state.zoom);
                        self.state.center_lon += step;
                    }
                    KeyCode::Up => {
                        let step = 6.0 / 2.0_f64.powf(self.state.zoom);
                        self.state.center_lat += step;
                    }
                    KeyCode::Down => {
                        let step = 6.0 / 2.0_f64.powf(self.state.zoom);
                        self.state.center_lat -= step;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn set_center(&mut self, lat: f64, lon: f64) {
        self.state.center_lat = lat;
        self.state.center_lon = lon;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn braille_pixel_mask_matches_map_scii() {
        // (x%2,y%4) -> dot mask mapping (U+2800 + mask).
        assert_eq!(pixel_mask_for(0, 0), 0x01);
        assert_eq!(pixel_mask_for(1, 0), 0x08);
        assert_eq!(pixel_mask_for(0, 1), 0x02);
        assert_eq!(pixel_mask_for(1, 1), 0x10);
        assert_eq!(pixel_mask_for(0, 2), 0x04);
        assert_eq!(pixel_mask_for(1, 2), 0x20);
        assert_eq!(pixel_mask_for(0, 3), 0x40);
        assert_eq!(pixel_mask_for(1, 3), 0x80);
    }

    #[test]
    fn braille_frame_sets_expected_cell_mask() {
        let mut frame = BrailleFrame::new(3, 2);
        frame.set_pixel(0, 0);
        assert_eq!(frame.masks[0], 0x01);

        frame.set_pixel(1, 0);
        assert_eq!(frame.masks[0], 0x01 | 0x08);
    }

    #[test]
    fn ll2tile_center_at_zoom0() {
        let (x, y) = ll2tile(0.0, 0.0, 0);
        assert!((x - 0.5).abs() < 1e-6);
        assert!((y - 0.5).abs() < 1e-6);
    }
}

