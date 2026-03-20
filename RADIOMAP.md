# RadioMap + Spelman Handoff

This file is a handoff summary for your Spelman AI lead.

It explains:
- what was changed in Spelman,
- why those changes were made,
- how OpenRustMap + Radio-map plugin are intended to work in standalone vs embedded mode.

---

## 1) Big picture

The integration model was designed around one core idea:

- **OpenRustMap is both:**
  - a standalone terminal app, and
  - an embeddable map/widget library for host apps like Spelman.

- **Radio-map plugin can operate in different modes:**
  - plugin plays itself (standalone scenario),
  - host plays stream URL (Spelman integration scenario),
  - URL-only handoff mode.

In Spelman, the intended mode is:
- user picks a station in Radio-map popup,
- plugin emits station + stream URL,
- Spelman receives URL and plays it through its own audio engine.

---

## 2) What was changed in Spelman

## Cargo / dependencies

### `Spelman/Cargo.toml`

Added dependencies for:
- embedded OpenRustMap crates:
  - `openrustmap-core`
  - `openrustmap-plugin-radiobrowser`
- network stream playback:
  - `reqwest` (blocking + rustls)

**Why:**  
Spelman needed to compile against the OpenRustMap widget/plugin API and add URL-stream decoding support.

---

## Keybinding + action wiring

### `Spelman/src/config/settings.rs`

Added new bindable action:
- `ToggleOpenRustMap`

Default keybinding set to:
- `m`, `M`

Action label + defaults list updated.

**Why:**  
Spelman needed a first-class, user-bindable way to open/close the OpenRustMap popup.

---

## Audio command/event model for streams

### `Spelman/src/util/channels.rs`

Added:
- `AudioCommand::PlayUrl(String)`

Changed playback start event:
- `AudioEvent::Playing` now includes:
  - `path: Option<PathBuf>`
  - `url: Option<String>`

**Why:**  
File-only playback was too narrow. URL playback requires a distinct command path, and event payload needed to represent either file-backed or URL-backed media.

---

## Stream decoder support

### `Spelman/src/audio/decoder.rs`

Added:
- `AudioDecoder::open_url(url: &str)`
- `HttpBufferedSource` implementing `Read + Seek + MediaSource` over HTTP response buffering

**Why:**  
Says in plain terms: Symphonia expects a seekable media source. HTTP streams are not naturally seekable, so buffering wrapper was introduced to make URL decoding possible with the existing audio pipeline.

---

## Audio playback bridge + engine routing

### `Spelman/src/audio/bridge.rs`

Updated file playback event shape to new optional fields.

Added:
- `start_playback_url(...)`

**Why:**  
Needed dedicated URL playback initializer parallel to file playback initializer.

### `Spelman/src/audio/engine.rs`

Added handling for:
- `AudioCommand::PlayUrl(url)` in idle and active states.

**Why:**  
Engine loop had to recognize stream URL commands and spin up URL-backed playback without breaking existing file behavior.

---

## Coordinator/player state updates

### `Spelman/src/coordinator/player.rs`

Updated `AudioEvent::Playing` match arm to handle optional file path / URL layout.

Seek behavior adjusted for stream cases:
- no-op seek operations when duration is unknown/zero.

**Why:**  
Streams often don’t have reliable total duration or random seek support. This prevents bad UX and invalid seek calls.

---

## Playing tab behavior for streams

### `Spelman/src/ui/tabs/playing.rs`

Adjusted empty-state logic so stream playback doesn’t appear as “no track loaded” when `file_path` is absent.

**Why:**  
A URL stream can be actively playing even without a filesystem path.

---

## OpenRustMap popup embedding

### `Spelman/src/app.rs`

Added embedded state:
- `openrustmap_visible`
- `openrustmap_widget`
- `radiobrowser_popup`

Added key action handling:
- `ToggleOpenRustMap` opens/closes modal popup.

Added rendering:
- OpenRustMap popup rendered as overlay.
- RadioBrowser popup rendered inside it.

Added input routing while popup is active:
- keyboard chars/backspace/enter
- scroll
- map movement inputs
- popup close handling with `Esc`

Added station selection handling:
- on `PluginEvent::StationSelected`:
  - center map if coordinates exist
  - if action is `PlayInHost`, send `AudioCommand::PlayUrl(stream_url)` to Spelman engine

**Why:**  
This is the core host-integration behavior: OpenRustMap acts as a station/map selector, Spelman remains the playback authority.

---

## 3) Why these changes were made (architectural intent)

The intent was to keep responsibilities clean:

- **OpenRustMap / plugin**
  - map UI
  - station discovery/selection
  - emits semantic events (`StationSelected`)

- **Spelman**
  - playback lifecycle
  - audio engine ownership
  - stream decoding and transport

This keeps OpenRustMap reusable across hosts and keeps Spelman in control of audio features (EQ, future AI, queue logic, etc.).

---

## 4) Current status and caveats

- The integration compiles and the command/event path exists.
- It is still a first-pass implementation and should be considered **integration scaffolding**, not final polish.
- Stream buffering/seek behavior is intentionally conservative for now.

---

## 5) Recommended review focus for Spelman AI lead

- Confirm `PlayUrl` path and URL decoder behavior match Spelman’s playback standards.
- Verify desired UX for stream metadata/title display.
- Decide whether to keep or refactor the current event-routing blocks in `app.rs` (there is repeated routing logic).
- Validate thread safety/perf of buffered HTTP source under long-running streams.

---

## 6) Integration contract summary

For embedded host mode (Spelman):
- OpenRustMap plugin emits station selection event.
- Event includes stream URL and optional geo coordinates.
- Host (Spelman) decides whether/how to play stream URL.

For standalone OpenRustMap mode:
- Plugin may play itself, use external player, or URL-only.

That split is intentional and is the core product design.
