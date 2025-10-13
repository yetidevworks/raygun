# Running Raygun

Raygun exposes the same HTTP interface as the Spatie Ray desktop app. The
binary bundles an HTTP server and terminal UI in a single process.

```bash
cargo run
```

## Bind Address

- `--bind <addr>` (defaults to `0.0.0.0:23517`)
- Environment alternative: `RAYGUN_BIND=0.0.0.0:23517`

Use the address you configure in your PHP project's Ray settings. Once the app
is running, invoke the usual `ray()` helper and payloads will appear in the
timeline list.

## Development Tips

1. Keep one terminal per workspace: one for `cargo watch -x 'run -- --bind …'`
   and another to run unit tests.
2. Navigate the timeline with `↑/↓` or `j/k`; `PgUp/PgDn` jump 10 entries. Use
   `Tab` to focus the details pane (same keys to scroll) and `Ctrl+L` to cycle
   layout presets. While on the details pane use `Enter`/`→` to expand, `←` to
   collapse, and `Space` to toggle. `Ctrl+K` clears the timeline, `Ctrl+D`
   toggles the raw payload viewer, `f` cycles the color filter, `?` opens the
   help overlay, and you can quit with `q`, `Esc`, or `Ctrl+C`.
3. If the port is already in use, Raygun fails to bind; choose another port via
   `--bind 127.0.0.1:23518` while testing.
