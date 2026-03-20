# OpenRustMap — Security & architecture audit

**Scope:** `/home/jp/Code/OpenRustMap` — `openrustmap-core`, `openrustmap-plugin-radiobrowser`, `openrustmap-app` (Rust sources only).  
**Excluded:** `target/` build artifacts.  
**Date:** 2025-03-20  
**Role:** Rust / security-oriented read-only review (no code changes in this pass).

---

## Executive summary

OpenRustMap is a **small TUI map widget** (vector tiles → Braille rendering) plus an optional **Radio-Browser** search popup. It uses **blocking `reqwest`** with **rustls**, **gzip** decompression for tiles, on-disk tile cache under XDG cache, and **serde** for Radio-Browser JSON APIs.

Positives: **no `unsafe`** in scanned sources, TLS defaults are sane, URLs are built from numeric tile coordinates or encoded query strings (no raw path concatenation from users for tiles).

Main risks: **unbounded download/decompress** for tiles (memory DoS), **vector tile parser panics** on malicious PBF (availability), **configurable tile base URL** (SSRF / policy if pointed at internal services), and **JSON from Radio-Browser** driving **`stream_url` values** passed to hosts (e.g. Spelman). Minor: **unused `tokio` dependency** in the radiobrowser plugin.

---

## Findings by severity

### Critical

| ID | Issue | Notes |
|----|--------|------|
| C1 | **No limit on tile HTTP body size** | `TileSource::get_tile_blocking` in `openrustmap-core/src/tile_source.rs` uses `resp.bytes()` then `to_vec()` with **no max length**. A malicious or compromised endpoint can return a multi‑gigabyte body → **OOM**. Same class of bug as unbounded HTTP buffers in audio players. |

### High

| ID | Issue | Notes |
|----|--------|------|
| H1 | **Unbounded gzip decompression** | `maybe_decompress_gzip` uses `read_to_end` into a `Vec` with **no cap** → **gzip bomb** / decompression DoS if content is hostile. |
| H2 | **Vector tile parse may panic** | `openrustmap_widget.rs` uses `VectorTile::new` + `read_layers()` on **network-sourced** bytes. The crate’s own test (`tile_source.rs` tests) documents that the parser **can panic** on invalid payloads — caught with `catch_unwind` only in tests, **not in production render path** → one bad tile could **crash the embedding process**. |

### Medium

| ID | Issue | Notes |
|----|--------|------|
| M1 | **User-configurable `base_url`** | `TileSourceConfig.base_url` can be pointed at **any HTTP(S) origin** (internal IPs, metadata services). Mostly a **policy / SSRF-adjacent** concern for locked-down environments; not internet-facing RCE by itself. |
| M2 | **Radio-Browser JSON → `stream_url`** | `RadioBrowserPopup::search_now` / `fetch_nearby_now` deserialize `url` / `url_resolved` into `StationInfo.stream_url` and pass to hosts. **Compromised API or MITM** (if TLS broken) could supply odd schemes; `reqwest` in hosts typically restricts schemes — still **trust boundary** is “API + TLS”. |
| M3 | **In-memory tile cache clones full tile** | `mem_cache.get(&key)` returns `Ok(v.clone())` — doubles short-term memory on hits; combined with large tiles exacerbates C1. |

### Low

| ID | Issue | Notes |
|----|--------|------|
| L1 | **`unwrap()` on HTTP client build** | `RadioBrowserPopup::new` uses `.unwrap()` on `reqwest::blocking::Client::builder()...build()` — panics if TLS init fails (rare). |
| L2 | **Unused dependency** | `openrustmap-plugin-radiobrowser/Cargo.toml` lists **`tokio`** but plugin code uses only **blocking** `reqwest` — unnecessary dependency surface for audits and compile time. |
| L3 | **`serde_json` in plugin Cargo.toml** | **Confirmed unused** in `openrustmap-plugin-radiobrowser/src` (no `serde_json::` imports; `resp.json()` deserializes via serde + `reqwest`). Safe to remove from `Cargo.toml`. |
| L4 | **Disk cache writes raw decompressed bytes** | Correct for reuse, but no quota → **disk fill** over long runs on untrusted endpoints (low severity for typical use). |

### Informational

- **`RadioBrowserPlugin::station_picked`** maps `RadioPlayMode::PlayInPlugin` to `StationSelectedAction::UrlOnly` with a comment that v1 is coarse — **behavioral inconsistency** between plugin types; hosts must document what they do with `stream_url`.
- **`openrustmap-app` settings** — `settings.toml` under `~/.config/openrustmap/` is minimal (`radio_map_enabled`); parse failures fall back silently (similar to many desktop apps).
- **Rendering cost** — `render_frame` iterates fixed 3×3 tile window but MVT feature loops can be heavy on dense tiles (performance / DoS via “heavy” legitimate tiles), separate from memory limits.

---

## Relationship to Spelman

Spelman embeds **`openrustmap-core`** and **`openrustmap-plugin-radiobrowser`**. Issues **C1/H1/H2** here interact with embedded-app stability: a bad tile fetch should **not** take down the music player — prefer **size limits + non-panicking parse path or catch at widget boundary** (with degraded map).

Cross-reference: **`~/Code/Spelman/AUDIT.md`**.

---

## Suggested unit & integration tests

### `openrustmap-core`

1. **`mod_wrap` / `ll2tile` / `base_zoom`** — already partially covered in `openrustmap_widget.rs` tests; add edge cases for negative longitude wrapping, poles (lat clamp), zoom boundaries.
2. **`TileSource::maybe_decompress_gzip`** — small valid gzip; non-gzip passthrough; **truncated gzip** returns error (no panic).
3. **Gzip bomb guard (after implementation)** — decompress with **max output length**; test exceeds limit → error.
4. **HTTP body limit (after implementation)** — mock server returns `Content-Length: huge` or chunked infinite stream; client aborts under cap (integration test with `tiny_http` or similar).
5. **`tile_url2`** — produces expected path shape; base URL with/without trailing slash (already normalized in `new`).
6. **Parser panic containment** — if upstream `open-vector-tile` still panics on bad input, add a **single** integration test that feeds known-bad bytes inside `catch_unwind` and assert widget **does not** propagate panic to thread boundary (or fix parser usage).

### `openrustmap-plugin-radiobrowser`

7. **`RadioBrowserPlugin::station_picked`** — table-driven test for each `RadioPlayMode` → expected `StationSelectedAction` and `PluginEvent` shape.
8. **`emit_station` / `action_for_play_mode`** — `PlayInHost` vs `UrlOnly` vs `PlayInPlugin` (document expected host behavior).
9. **URL building** — `search_now` builds query with `urlencoding::encode` (special characters, spaces) — unit test string without network (refactor to pure function if needed).
10. **Rate limit branch** — `last_search_at` / 900ms window: mock time or inject clock trait for deterministic tests (optional).

### `openrustmap-app`

11. **Settings load/save** — round-trip `AppSettings` to temp file; corrupt TOML keeps defaults.
12. **Rect helpers** — `centered_popup` never exceeds parent `area` (existing `min` — golden cases).

### End-to-end (optional)

13. **Smoke** — `OpenRustMapWidget::new(OpenRustMapConfig::default())` succeeds on CI with network **disabled** if you inject a `TileSource` test double (requires refactor for dependency injection).

---

## Action priority (for agents)

1. **P0:** Max tile download size + max gzip decompress output; fail closed (C1, H1).  
2. **P1:** Contain or eliminate **panic** from MVT parse on untrusted bytes (H2).  
3. **P2:** Document or restrict `base_url` (allowlist, or warn on non-HTTPS) for enterprise users (M1).  
4. **P3:** Remove unused **`tokio`** / **`serde_json`** from plugin if confirmed unused (L2, L3).  
5. **P4:** Replace `unwrap()` on `Client::build` with `expect` + context or propagate `Result` from `new()` (L1).

---

*End of OpenRustMap audit.*
