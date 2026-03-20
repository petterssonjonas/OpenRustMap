# Spelman Follow-ups (Suggested, Not Applied)

These are recommended next changes for Spelman.  
I did **not** apply these in this pass.

---

## 1) Refactor duplicated OpenRustMap event routing in `app.rs`

Current integration routes station events from multiple handlers (`char`, `enter`, `scroll`, etc.) with repeated blocks.

### Suggested change
- Add a single helper:
  - `fn process_openrustmap_events(&mut self)`
- Call it from all relevant input paths.

### Why
- Lower bug risk.
- Easier to evolve plugin events (future event types beyond `StationSelected`).

---

## 2) Add explicit stream-mode state in `PlayingState`

Current stream handling infers behavior from optional path/duration.

### Suggested change
- Add:
  - `source_kind: File | StreamUrl`
  - `stream_url: Option<String>`
- Render dedicated “Live Stream” indicators in Playing tab.

### Why
- Cleaner UI logic than deriving stream mode from `file_path == None`.

---

## 3) Better stream metadata updates

Many live stations provide ICY metadata (current song/artist).

### Suggested change
- Introduce metadata update events from decoder/engine layer.
- Update `PlayingState.title/artist` live.

### Why
- Improves polish and makes radio mode feel native in Spelman.

---

## 4) Harden HTTP stream source behavior

Current buffered HTTP source is functional but minimal.

### Suggested change
- Add bounded buffer policy (avoid unbounded memory growth).
- Add reconnect/failure strategy for flaky streams.
- Add stream timeout/keepalive configuration.

### Why
- Long-running streams need resilience and predictable memory behavior.

---

## 5) Formalize plugin-host boundary (optional)

Spelman currently links OpenRustMap crates directly.

### Suggested change
- Introduce a thin integration module in Spelman:
  - `src/integrations/openrustmap.rs`
- Keep all OpenRustMap-specific code there, expose a minimal internal interface to `app.rs`.

### Why
- Keeps `app.rs` smaller and easier to maintain.
- Makes it easier to swap integration behavior later.

---

## 6) Tests to add in Spelman

### Suggested tests
- Unit:
  - `AudioCommand::PlayUrl` engine dispatch behavior.
  - no-op seek when duration is zero (stream mode).
- Integration:
  - popup open/select flow dispatches `PlayUrl`.
  - closing popup via `Esc` restores normal input routing.

### Why
- Prevent regressions as OpenRustMap + Spelman evolve independently.
