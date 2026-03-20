use crossterm::event::Event;
use ratatui::{
    buffer::Buffer,
    layout::{Rect},
};

/// Simplified host input events forwarded into OpenRustMap.
///
/// We intentionally keep this minimal for v1; more controls can be added
/// without breaking host apps.
#[derive(Debug, Clone)]
pub enum MapInput {
    CEvent(Event),
    Tick,
}

/// TUI widget interface intended to be embedded by other terminal apps.
pub trait MapWidget {
    /// Render map into the provided buffer/area.
    fn render(&mut self, area: Rect, buf: &mut Buffer);

    /// Forward input from the host event loop.
    fn handle_input(&mut self, input: MapInput);

    /// Programmatically set map center (used by the Radio-Browser plugin).
    fn set_center(&mut self, lat: f64, lon: f64);
}

