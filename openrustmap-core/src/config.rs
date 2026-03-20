use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Default vector-tile base URL used for v1 best-effort rendering.
///
/// Note: This can and should be overridden by config in real usage.
pub const DEFAULT_TILE_BASE_URL: &str = "https://mapscii.me/";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TileSourceConfig {
    /// Vector tile base URL, where `${z}/${x}/${y}.pbf` is appended.
    pub base_url: String,
    /// Optional local cache directory for downloaded tiles.
    pub cache_dir: Option<PathBuf>,
}

impl Default for TileSourceConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_TILE_BASE_URL.to_string(),
            cache_dir: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenRustMapConfig {
    pub tile_source: TileSourceConfig,
    pub initial_center_lat: f64,
    pub initial_center_lon: f64,
    pub initial_zoom: f64,
    /// Render Braille by default (otherwise ASCII block mode).
    pub braille: bool,
}

impl Default for OpenRustMapConfig {
    fn default() -> Self {
        Self {
            tile_source: TileSourceConfig::default(),
            // Berlin-ish, same general vibe as MapSCII defaults.
            initial_center_lat: 52.51298,
            initial_center_lon: 13.42012,
            initial_zoom: 4.0,
            braille: true,
        }
    }
}

impl TileSourceConfig {
    /// Resolve cache dir, creating the directory on demand.
    pub fn resolved_cache_dir(&self) -> Option<PathBuf> {
        if let Some(dir) = self.cache_dir.clone() {
            Some(dir)
        } else {
            BaseDirs::new().map(|d| d.cache_dir().join("openrustmap"))
        }
    }
}

