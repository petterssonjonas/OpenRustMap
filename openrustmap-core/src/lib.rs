//! Shared OpenRustMap types and widget interfaces.

pub mod config;
pub mod plugin;
pub mod widget;
pub mod tile_source;
pub mod openrustmap_widget;

pub use config::{OpenRustMapConfig, TileSourceConfig, DEFAULT_TILE_BASE_URL};
pub use plugin::{
    PluginEvent, RadioPlayMode, StationInfo, StationSelectedAction,
};
pub use widget::{MapInput, MapWidget};
pub use openrustmap_widget::{MapViewState, OpenRustMapWidget, MAX_VIEW_ZOOM};
