# Task 7 Report: Remove Terminal Mode

## Scope

- Removed the PTY terminal implementation and UI pane (`src/terminal.rs`, `src/ui/terminal.rs`).
- Removed terminal tabs, focus, geometry, key-byte conversion, mouse routing, notices, status rows, and `/terminal`.
- Kept the full-frame conversation document, composer, execution-card hitboxes, selection, sticky question, return-to-bottom control, and Tasks status/composer paths.
- Removed the direct `portable-pty` and `vt100` dependencies without touching the vendored directory.
- Updated the two architecture mirrors to remove the deleted module reference.

## RED

- `cargo test slash_commands_do_not_include_terminal -- --nocapture` failed because `/terminal` remained registered.
- `cargo test former_terminal_shortcuts -- --nocapture` failed because Ctrl+T and Alt+N remained terminal actions.
- `cargo test document_viewport_and_composer_use_the_full_height -- --nocapture` passed before the change because the pre-removal test fixture had the terminal hidden. The test remains the full-frame geometry regression after terminal state removal.

## GREEN

- The main loop now uses `size.height` for document, composer, return-bottom, and execution-card geometry; it no longer has terminal resize, update, or tab-hitbox work.
- Ctrl+T and Alt+N map to `KeyAction::Char`; Ctrl+C remains quit/cancel, Ctrl+X remains cut, and navigation mappings remain unchanged. The app-level regression proves Ctrl+T then Alt+N writes `tn` to the chat buffer.
- The renderer invokes `render_document` over `frame.area()` after overlays, with no pane split.
- General status and dashboard no longer display Terminal or Shell tool data. Tasks still render from task snapshots and no longer have a terminal-tab label.
- Subagent event draining moved into `update_tasks`, preserving task transcript and Task Wait processing after the terminal tick loop was deleted.

## Verification

- Focused GREEN: slash-command, shortcut-mapping, full-height, shortcut-to-input-buffer, and status removal tests passed.
- `cargo test mouse_ -- --nocapture`: 14 passed.
- `cargo fmt --check`, `cargo check`, `cargo clippy -- -D warnings`: passed.
- `cargo test`: 323 passed, 2 ignored.
- `rg -n "TerminalTab|TerminalStatus|TerminalMouseScroll|terminal_visible|terminal_focused|terminal_top_row|/terminal" src prompts/system.md` leaves only two intentional negative assertions for removal coverage.
- `git diff --check`: passed.

## Residual Classification And Self-Review

- `Terminal` remains only as Ratatui/Crossterm's terminal backend type in `main.rs` and generic terminal event wording; it is not the removed PTY product surface.
- The exact terminal-mode sweep has no operational matches; `/terminal` survives only in negative tests proving removal.
- `vendor/vt100` was intentionally retained, as it is outside this task's deletion scope; Cargo no longer depends on it directly.
- The only judgment call outside the brief file list was removing the dashboard's Terminal metric, because it depended on the deleted terminal status helper and would otherwise leave a visible terminal label.
