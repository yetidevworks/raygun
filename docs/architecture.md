# Raygun Architecture Blueprint

## Vision
- Deliver a cross-platform Rust binary that emulates the Spatie Ray desktop experience inside the terminal.
- Combine a responsive TUI with an embedded HTTP server so PHP (and other) clients can stream debugging payloads to the app in real time.
- Showcase modern terminal capabilities: rich layouting, animations, syntax highlighting, and command palette style interactions.

## Core System

### Runtime Foundations
- **Async runtime**: `tokio` for ergonomics and ecosystem support.
- **HTTP ingress**: `axum` (with `hyper`) to accept Ray payloads on `localhost:23517`; expose health + lock endpoints expected by Ray clients.
- **Message transport**: use `tokio::sync::broadcast` for fan-out (UI, persistence, plugins) and `mpsc` channel for ordered ingestion.
- **State store**: central `AppState` guarded by `tokio::sync::RwLock`, persisting sessions, screens, timers, and locks.
- **Serialization**: `serde` + `serde_json` to model Ray payload/meta structures (mirroring PHP `Payload` classes).
- **Error reporting**: `color-eyre` for rich diagnostics during development.

### Terminal Interface
- **Rendering**: `ratatui` (successor to `tui-rs`) with `crossterm` backend for portability.
- **Styling**: theming layer built on `ratatui::style`, tuned for dark/light; adopt Spatie Ray color tokens.
- **Syntax highlighting**: `syntect` with lazy-loaded themes; fall back to inline style heuristics for performance.
- **Input**: `crossterm` event stream + custom keymap (modal navigation, command palette, fuzzy filters).
- **Widgets**:
  - Timeline panel (virtualized list) highlighting payload types and severity.
  - Details panel with tabs (raw JSON, pretty, stack trace, diff view).
  - Screen navigator (Ray “screens”), search box, filter chips, status footer.
  - Command palette + quick actions (clear, focus project, toggle auto-scroll).
- **Animations**: leverage `ratatui::widgets::LineGauge` / transitions with frame interpolation to hint state changes.

### Concurrency Model
1. `axum` routes parse incoming JSON into `RayEvent`.
2. Events pass through validation (unknown payload detection) and metadata enrichment (timestamps, client fingerprint).
3. Primary event loop consumes from `mpsc` queue, updates `AppState`, and notifies subscribers.
4. UI task drives `ratatui` terminal redraw at ~60 FPS or throttled by diffing engine.
5. Background workers:
   - Persistence (optional) writing session logs.
   - Rate-limit guard to mirror Ray desktop notifications.
   - Plugin hooks (stdout forwarder, webhook, etc.) via async traits.

## Feature Roadmap

### Milestone 1 – Foundations
- Scaffold crate, wire Tokio runtime, configure structured logging.
- Implement minimal HTTP server: `_availability_check`, POST `/` (payload ingest), and GET `/locks/:name`.
- Parse `Request` + core payloads (`Log`, `Text`, `Custom`, `Bool`, `Table`, `Trace`, `Exception`).
- Stand up basic TUI layout with timeline list + details pane fed by mock events.

### Milestone 2 – Rich UX
- Full keyboard navigation, command palette, project/screen filtering.
- Syntax-highlighted bodies, virtual scrolling, sticky search.
- Implement Ray “screens” semantics and `ClearAll`, `Remove`, `Hide` actions.
- Visual indicators for timers, measures, and notifications.

### Milestone 3 – Ecosystem Integration
- Persist sessions to disk (sqlite or JSONL) with replay on startup.
- Export/share events (copy as JSON, send to clipboard).
- Plugin SDK (dynamic dispatch) to let users subscribe to event bus.
- Cross-platform packaging (binary release, `cargo xtask release`, optional `brew`/`scoop` recipes).

### Milestone 4 – Polish & Advanced Features
- Live diff for successive payloads, inline markdown rendering, image previews (kitty/iTerm/wezterm protocols).
- Remote forwarding (SSH tunneling helper) and TLS support.
- Telemetry (opt-in) for crash reports, update checks.
- Internationalization scaffold and accessibility (high-contrast mode, screen reader friendly export).

## Risks & Mitigations
- **Protocol drift**: mirror PHP payload schema with snapshot tests against fixtures from `spatie/ray`. Automate with GitHub actions.
- **Performance**: large payloads (tables, traces). Use incremental rendering, chunked diffing, and limit highlight passes.
- **Terminal capability variance**: detect feature support (truecolor, hyperlinks, inline images) and gracefully degrade.
- **Concurrency complexity**: enforce single writer principle for `AppState`; encapsulate mutations behind typed commands.

## Next Steps
1. `cargo init --bin raygun` and configure crates (`tokio`, `axum`, `ratatui`, `serde`, `color-eyre`).
2. Model `RayRequest`, `RayPayload` enums with serde tagging.
3. Implement async app skeleton: HTTP listener + UI loop stub with crossbeam channel.

