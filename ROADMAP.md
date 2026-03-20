# OpenRustMap Roadmap

## Recently shipped
- **Radio-map playback (v1.1):** native pause (`p`), volume (`+` / `-`), decode/output errors in the status line, external player mode (`m` cycles PlayInPlugin → UrlOnly → ExternalPlayer, or Settings `e` to prefer external on open). Executable: `OPENRUSTMAP_RADIO_PLAYER` (default `mpv`).
- **Mouse:** terminal mouse capture enabled; map scroll-zoom and right-drag pan are suppressed while Settings or Radio-map is open; in Radio-map, wheel moves selection, left-click selects a row, right-click plays.

## Vision
- A polished terminal world-map app that runs as its own product.
- Plugin-driven features (starting with Radio-map), enabled from settings when installed.
- Spelman remains fully separate; any similar radio search there is independent and non-map.

## Near-Term (v1.x)
- Add map color styling where data supports it:
  - water/sea in blue
  - land in green
  - desert/arid regions in tan
  - fallback: if biome/landclass data is missing, prioritize placenames and linework
- Improve map labels:
  - zoomed all the way out: no placenames
  - zoom in: country names
  - zoom in more: states/oblasts/regions
  - zoom in further: city names, then denser local labels
  - progressive label density by zoom with overlap control
- Add map detail levels in settings:
  - `Low`: country outlines + water
  - `Normal`: current roads/water/land
  - `High`: more roads + places + richer overlays
- Improve globe view:
  - smoother full-world projection at min zoom
  - adaptive rendering based on terminal size
- Expand plugin host:
  - discover plugins from `~/.config/openrustmap/plugins`
  - enable/disable plugins in settings
  - dynamic top menu items per installed plugin

## Mid-Term (v2)
- Better rendering pipeline:
  - async tile loading + prefetch queue
  - improved line styling and polygon fill quality
  - label collision avoidance
- Topographic modes:
  - hillshade-like contour layer
  - terrain tinting where tile source supports it
- Radio-map improvements:
  - better geo fallback if station lacks coordinates
  - station filtering by language/tag/bitrate
  - ~~internal playback profiles~~ (shipped in v1.1 — extend with UI polish / ffmpeg-free AAC edge cases)

## Long-Term (v3+)
- Configurable style packs (themes).
- Additional plugin SDK docs and examples.
- Optional offline tile packs / MBTiles source support.
- Multi-pane workflows for embedding hosts.
