use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RadioPlayMode {
    /// OpenRustMap Radio-map plugin should play the stream (cpal + symphonia).
    PlayInPlugin,
    /// Do not play; only provide stream URL + station info.
    UrlOnly,
    /// Hand off playback to an external process (e.g. `mpv --no-video <url>`).
    ExternalPlayer,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StationInfo {
    pub station_uuid: String,
    pub name: String,
    pub stream_url: String,
    pub geo_lat: Option<f64>,
    pub geo_long: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StationSelectedAction {
    /// Radio-map plugin should start playing `stream_url` (native engine).
    PlayInPlugin,
    /// Do not play; only show URL/info.
    UrlOnly,
    /// Host/plugin should start an external player for `stream_url`.
    ExternalPlayer,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StationSelected {
    pub station: StationInfo,
    pub action: StationSelectedAction,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PluginEvent {
    StationSelected(StationSelected),
}

