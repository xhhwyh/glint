# Synchronized Draw Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make each Ratatui frame appear atomically so scrolling long Bash output cannot leave persistent partial-frame artifacts such as repeated `(fetch)` suffixes.

**Architecture:** Add one generic draw wrapper around `Terminal<CrosstermBackend<W>>`. It queues Crossterm's begin-synchronized-update marker, runs `Terminal::try_draw`, always emits the end marker, and preserves the draw error when both drawing and ending fail.

**Tech Stack:** Rust 2024, Ratatui 0.29, Crossterm 0.28.

**Spec:** `docs/superpowers/specs/2026-08-25-inline-execution-cards-design.md`

## Global Constraints

- Keep the existing 40-millisecond event polling cadence.
- Do not change transcript contents, scrolling semantics, or terminal layout in this plan.
- Unsupported terminal emulators must retain current behavior by ignoring the synchronization escape sequences.
- The end marker must be attempted after both successful and failed draws.
- Preserve the user's unrelated `config.yaml` modification.

---

### Task 1: Synchronized Ratatui Draw Boundary

**Files:**
- Modify: `src/main.rs:1-120`
- Test: `src/main.rs` test module

**Interfaces:**
- Consumes: `crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate}` and `ratatui::Terminal::try_draw`.
- Produces: `fn draw_synchronized<W, F, E>(terminal: &mut Terminal<CrosstermBackend<W>>, render: F) -> io::Result<()>` where `W: Write`, `F: FnOnce(&mut Frame) -> Result<(), E>`, and `E: Into<io::Error>`.

- [ ] **Step 1: Write the failing boundary tests**

Add a cloneable recording writer so the emitted control sequences can be inspected without enabling Ratatui's `backend-writer` feature:

```rust
#[derive(Clone, Default)]
struct RecordingWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl RecordingWriter {
    fn output(&self) -> String {
        let bytes = self.0.lock().unwrap().clone();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl Write for RecordingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn synchronized_draw_emits_begin_and_end_markers() {
    let writer = RecordingWriter::default();
    let backend = CrosstermBackend::new(writer.clone());
    let mut terminal = Terminal::with_options(
        backend,
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 20, 4)),
        },
    )
    .unwrap();

    draw_synchronized(&mut terminal, |_frame| io::Result::Ok(())).unwrap();

    let output = writer.output();
    assert!(output.contains("\x1b[?2026h"));
    assert!(output.contains("\x1b[?2026l"));
}

#[test]
fn synchronized_draw_emits_end_marker_when_render_fails() {
    let writer = RecordingWriter::default();
    let backend = CrosstermBackend::new(writer.clone());
    let mut terminal = Terminal::with_options(
        backend,
        ratatui::TerminalOptions {
            viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 20, 4)),
        },
    )
    .unwrap();

    let error = draw_synchronized(&mut terminal, |_frame| {
        io::Result::<()>::Err(io::Error::other("render failed"))
    })
    .unwrap_err();

    assert_eq!(error.to_string(), "render failed");
    let output = writer.output();
    assert!(output.contains("\x1b[?2026h"));
    assert!(output.contains("\x1b[?2026l"));
}
```

- [ ] **Step 2: Run the focused tests and verify they fail**

Run:

```bash
rtk cargo test synchronized_draw -- --nocapture
```

Expected: compilation fails because `draw_synchronized` does not exist.

- [ ] **Step 3: Implement the minimal synchronized wrapper**

Import the commands and command traits, then add:

```rust
fn draw_synchronized<W, F, E>(
    terminal: &mut Terminal<CrosstermBackend<W>>,
    render: F,
) -> io::Result<()>
where
    W: Write,
    F: FnOnce(&mut ratatui::Frame) -> Result<(), E>,
    E: Into<io::Error>,
{
    use crossterm::{ExecutableCommand, QueueableCommand};
    use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};

    terminal
        .backend_mut()
        .queue(BeginSynchronizedUpdate)?;
    let draw_result = terminal.try_draw(render).map(|_| ());
    let end_result = terminal
        .backend_mut()
        .execute(EndSynchronizedUpdate)
        .map(|_| ());

    match (draw_result, end_result) {
        (Err(draw_error), _) => Err(draw_error),
        (Ok(_), Err(end_error)) => Err(end_error),
        (Ok(()), Ok(())) => Ok(()),
    }
}
```

Replace the main-loop draw call with:

```rust
draw_synchronized(terminal, |frame| {
    ui::render(frame, &app);
    io::Result::Ok(())
})?;
```

- [ ] **Step 4: Run focused and repository checks**

Run:

```bash
rtk cargo test synchronized_draw -- --nocapture
rtk cargo fmt --check
rtk cargo check
rtk git diff --check
```

Expected: both synchronized-draw tests pass and all commands exit successfully.

- [ ] **Step 5: Commit the isolated rendering fix**

```bash
rtk git add src/main.rs
rtk git commit -m "fix(ui): synchronize terminal frame updates" -m "- bracket each Ratatui draw with synchronized-update markers
- close the update boundary even when rendering fails"
```
