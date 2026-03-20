# RadioMap Executive Summary

## Purpose

This is a short handoff for Spelman AI leadership.

Goal: integrate OpenRustMap + Radio-map so station selection can either:
- play in OpenRustMap (standalone mode), or
- hand stream URL to Spelman for playback (embedded host mode).

---

## What was changed in Spelman (high level)

- Added OpenRustMap dependencies in `Cargo.toml`:
  - `openrustmap-core`
  - `openrustmap-plugin-radiobrowser`
- Added URL playback support:
  - `AudioCommand::PlayUrl(String)`
  - URL-aware `AudioEvent::Playing` payload (`path`/`url` optional fields)
- Added network stream decode path:
  - `AudioDecoder::open_url(...)`
  - HTTP buffered source compatible with Symphonia
- Extended engine + bridge:
  - route `PlayUrl` through audio startup pipeline
- Added OpenRustMap popup in app layer:
  - new bindable action `ToggleOpenRustMap` (`m`/`M`)
  - popup render + input routing
  - handle `StationSelected` event and send `PlayUrl` to Spelman when action is `PlayInHost`
- Stream UX safety:
  - disable seek logic when duration is unknown/zero (common for live streams)
  - avoid “no track loaded” empty-state during active stream playback

---

## Why these changes were made

To keep responsibilities clean:

- **OpenRustMap / Radio-map plugin**
  - map UI
  - station selection
  - emits station + stream URL event

- **Spelman**
  - remains playback authority
  - handles stream decode/playback, EQ, queue, and player UX

This makes OpenRustMap reusable in multiple hosts while preserving Spelman’s control over audio behavior.

---

## Integration contract (important)

Embedded host mode (Spelman):
- plugin emits `StationSelected { stream_url, geo... }`
- host decides playback behavior
- in current integration, `PlayInHost` triggers `AudioCommand::PlayUrl(stream_url)`

Standalone OpenRustMap mode:
- plugin can support self-play / external-player / URL-only behavior by settings

---

## Current maturity

- Works as integration scaffold and compiles.
- Not final polish yet.
- Recommended next steps are documented in:
  - `SPELMAN_FOLLOWUPS.md`
  - `RADIOMAP.md` (full technical rationale)

