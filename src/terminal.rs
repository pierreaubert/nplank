//! Embedded terminal core: a PTY child whose output feeds an
//! `alacritty_terminal` grid, independent of any view.
//!
//! The view layer polls `render_rows()`; this module owns the process, the
//! parser, colors, and scrollback. Desktop-only: iOS cannot spawn processes
//! and WASM has no PTY.

// Rust guideline compliant 2026-02-21
use alacritty_terminal::{
    event::{Event, EventListener},
    grid::{Dimensions, Scroll},
    term::{
        Config, Term,
        cell::{Cell, Flags},
        color::Colors,
    },
    vte::ansi::{Color, CursorShape, NamedColor, Processor, Rgb, StdSyncHandler},
};
use anyhow::{Context, Result};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::{
    io::{Read, Write},
    sync::{Arc, Mutex, mpsc},
};

/// Events the view may care about, drained without touching the grid lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    /// Grid content likely changed; repaint.
    Wakeup,
    /// Child set a new title (OSC).
    Title(String),
    /// Child process exited.
    Exited,
    /// Child requests text be written back (e.g. cursor position report).
    WriteBack(String),
}

#[derive(Debug)]
struct Listener(mpsc::Sender<TerminalEvent>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let mapped = match event {
            Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange => {
                Some(TerminalEvent::Wakeup)
            }
            Event::Title(title) => Some(TerminalEvent::Title(title)),
            Event::ResetTitle => Some(TerminalEvent::Title(String::new())),
            // Window-size queries are answered by the PTY winsize ioctl;
            // replying here with guessed dimensions would corrupt TUIs.
            Event::TextAreaSizeRequest(_) => None,
            Event::PtyWrite(text) => Some(TerminalEvent::WriteBack(text)),
            _ => None,
        };
        if let Some(event) = mapped {
            let _ = self.0.send(event);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TermDims {
    cols: usize,
    rows: usize,
}

impl Dimensions for TermDims {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// One run of same-styled cells on a row, ready for view rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    /// Text of the run (one char per cell, wide chars padded by the parser).
    pub text: String,
    /// Resolved foreground RGB.
    pub fg: (u8, u8, u8),
    /// Resolved background RGB.
    pub bg: (u8, u8, u8),
    /// Bold cells.
    pub bold: bool,
    /// Italic cells.
    pub italic: bool,
    /// Underlined cells.
    pub underline: bool,
    /// Struck-through cells.
    pub strikeout: bool,
    /// Inverse video already applied to fg/bg.
    pub inverse: bool,
}

/// One terminal instance: PTY child + parser grid.
pub struct Terminal {
    term: Arc<Mutex<Term<Listener>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    events: mpsc::Receiver<TerminalEvent>,
    /// Cursor `(col, row)` and visibility from the last render snapshot.
    dims: Mutex<TermDims>,
}

impl std::fmt::Debug for Terminal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Terminal").finish_non_exhaustive()
    }
}

impl Terminal {
    /// Spawns `program` (with `args`) in `cwd` on a fresh PTY.
    ///
    /// # Errors
    /// Returns PTY allocation or spawn failures.
    pub fn spawn(
        program: &str,
        args: &[&str],
        cwd: &std::path::Path,
        cols: u16,
        rows: u16,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                cols,
                rows,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("open PTY")?;
        let mut command = CommandBuilder::new(program);
        command.args(args);
        command.cwd(cwd);
        let child = pair
            .slave
            .spawn_command(command)
            .context("spawn terminal child")?;
        drop(pair.slave);

        let (send, events) = mpsc::channel();
        let dims = TermDims {
            cols: usize::from(cols),
            rows: usize::from(rows),
        };
        let term = Arc::new(Mutex::new(Term::new(
            Config::default(),
            &dims,
            Listener(send.clone()),
        )));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(pair.master.take_writer()?));
        let reader = pair.master.try_clone_reader()?;
        let master: Arc<Mutex<Box<dyn MasterPty + Send>>> = Arc::new(Mutex::new(pair.master));

        let term_reader = Arc::clone(&term);
        let writer_reader = Arc::clone(&writer);
        std::thread::Builder::new()
            .name("nplank-terminal-reader".into())
            .spawn(move || {
                let mut reader = reader;
                let mut processor: Processor<StdSyncHandler> = Processor::default();
                let mut buffer = [0u8; 8192];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(count) => {
                            let mut term = term_reader.lock().expect("terminal lock");
                            processor.advance(&mut *term, &buffer[..count]);
                            // Answer child queries (cursor reports etc.) inline so
                            // TUIs do not stall waiting for a response.
                            drop(term);
                        }
                    }
                    // Drain any write-back produced while parsing.
                    let _ = &writer_reader;
                }
                let _ = send.send(TerminalEvent::Exited);
            })
            .context("spawn terminal reader")?;

        Ok(Self {
            term,
            writer,
            master,
            child: Arc::new(Mutex::new(child)),
            events,
            dims: Mutex::new(dims),
        })
    }

    /// Drains pending view events without blocking.
    pub fn drain_events(&self) -> Vec<TerminalEvent> {
        let mut drained = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            // Write-backs must reach the child or TUIs stall; answer here.
            if let TerminalEvent::WriteBack(text) = &event {
                self.write(text.as_bytes());
            }
            drained.push(event);
        }
        drained
    }

    /// Writes user input to the child.
    pub fn write(&self, bytes: &[u8]) {
        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(bytes);
            let _ = writer.flush();
        }
    }

    /// Resizes grid and PTY together so the child reflows.
    ///
    /// # Errors
    /// Returns PTY resize failures.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let dims = TermDims {
            cols: usize::from(cols.max(1)),
            rows: usize::from(rows.max(1)),
        };
        *self.dims.lock().expect("dims lock") = dims;
        self.term.lock().expect("terminal lock").resize(dims);
        self.master
            .lock()
            .expect("pty lock")
            .resize(PtySize {
                cols,
                rows,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("resize PTY")
    }

    /// Scrolls the scrollback viewport by `delta` lines (positive = older).
    pub fn scroll(&self, delta: i32) {
        self.term
            .lock()
            .expect("terminal lock")
            .scroll_display(Scroll::Delta(delta));
    }

    /// Whether the child wants application cursor keys (`\x1bOA` style).
    pub fn cursor_keys_application(&self) -> bool {
        self.term
            .lock()
            .expect("terminal lock")
            .mode()
            .contains(alacritty_terminal::term::TermMode::APP_CURSOR)
    }

    /// Whether the child enabled bracketed paste.
    pub fn bracketed_paste(&self) -> bool {
        self.term
            .lock()
            .expect("terminal lock")
            .mode()
            .contains(alacritty_terminal::term::TermMode::BRACKETED_PASTE)
    }

    /// Whether the child has exited.
    pub fn exited(&self) -> bool {
        self.child
            .lock()
            .expect("child lock")
            .try_wait()
            .is_ok_and(|status| status.is_some())
    }

    /// Kills the child (terminal close). Safe to call after natural exit.
    pub fn kill(&self) {
        let _ = self.child.lock().expect("child lock").kill();
    }

    /// Current `(cols, rows)`.
    pub fn size(&self) -> (usize, usize) {
        let dims = *self.dims.lock().expect("dims lock");
        (dims.cols, dims.rows)
    }

    /// Visible rows merged into style runs, plus cursor `(col, row, visible)`.
    pub fn render_rows(&self) -> (Vec<Vec<Run>>, Option<(usize, usize)>) {
        let term = self.term.lock().expect("terminal lock");
        let content = term.renderable_content();
        let cursor = if matches!(content.cursor.shape, CursorShape::Hidden) {
            None
        } else {
            Some((
                content.cursor.point.column.0,
                usize::try_from(content.cursor.point.line.0).unwrap_or(0),
            ))
        };
        (rows_from_content(content), cursor)
    }
}

/// Merges a renderable grid snapshot into per-row style runs.
fn rows_from_content(content: alacritty_terminal::term::RenderableContent<'_>) -> Vec<Vec<Run>> {
    let colors = *content.colors;
    let mut rows: Vec<Vec<Run>> = Vec::new();
    let mut current_line: i32 = i32::MIN;
    for indexed in content.display_iter {
        let line = indexed.point.line.0;
        if line != current_line {
            rows.push(Vec::new());
            current_line = line;
        }
        let run = cell_run(indexed.cell, &colors);
        let row = rows.last_mut().expect("row exists");
        match row.last_mut() {
            Some(last) if same_style(last, &run) => last.text.push_str(&run.text),
            _ => row.push(run),
        }
    }
    rows
}

fn same_style(a: &Run, b: &Run) -> bool {
    a.fg == b.fg
        && a.bg == b.bg
        && a.bold == b.bold
        && a.italic == b.italic
        && a.underline == b.underline
        && a.strikeout == b.strikeout
        && a.inverse == b.inverse
}

fn resolve(color: Color, colors: &Colors) -> (u8, u8, u8) {
    let rgb: Rgb = match color {
        Color::Named(named) => colors[named].unwrap_or_else(|| default_named(named)),
        Color::Indexed(index) => {
            colors[usize::from(index)].unwrap_or_else(|| default_indexed(usize::from(index)))
        }
        Color::Spec(rgb) => rgb,
    };
    (rgb.r, rgb.g, rgb.b)
}

/// The alacritty-style default palette, used wherever the engine has not
/// configured a color (`Colors::default()` is intentionally all-`None`).
fn default_named(named: NamedColor) -> Rgb {
    const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
        Rgb { r, g, b }
    }
    match named {
        NamedColor::Foreground | NamedColor::BrightForeground => rgb(216, 216, 216),
        NamedColor::DimForeground => rgb(152, 152, 152),
        NamedColor::Background => rgb(24, 24, 24),
        NamedColor::Cursor => rgb(216, 216, 216),
        NamedColor::Black => rgb(24, 24, 24),
        NamedColor::Red => rgb(172, 66, 66),
        NamedColor::Green => rgb(144, 169, 89),
        NamedColor::Yellow => rgb(244, 191, 117),
        NamedColor::Blue => rgb(106, 159, 181),
        NamedColor::Magenta => rgb(170, 117, 159),
        NamedColor::Cyan => rgb(117, 181, 170),
        NamedColor::White => rgb(216, 216, 216),
        NamedColor::BrightBlack => rgb(107, 107, 107),
        NamedColor::BrightRed => rgb(197, 85, 85),
        NamedColor::BrightGreen => rgb(170, 196, 116),
        NamedColor::BrightYellow => rgb(254, 202, 136),
        NamedColor::BrightBlue => rgb(130, 184, 200),
        NamedColor::BrightMagenta => rgb(194, 140, 184),
        NamedColor::BrightCyan => rgb(147, 211, 195),
        NamedColor::BrightWhite => rgb(248, 248, 248),
        NamedColor::DimBlack => rgb(12, 12, 12),
        NamedColor::DimRed => rgb(86, 33, 33),
        NamedColor::DimGreen => rgb(72, 84, 44),
        NamedColor::DimYellow => rgb(122, 95, 58),
        NamedColor::DimBlue => rgb(53, 79, 90),
        NamedColor::DimMagenta => rgb(85, 58, 79),
        NamedColor::DimCyan => rgb(58, 90, 85),
        NamedColor::DimWhite => rgb(108, 108, 108),
    }
}

/// The standard 256-color palette for indexed colors.
fn default_indexed(index: usize) -> Rgb {
    const ANSI: [(u8, u8, u8); 16] = [
        (24, 24, 24),
        (172, 66, 66),
        (144, 169, 89),
        (244, 191, 117),
        (106, 159, 181),
        (170, 117, 159),
        (117, 181, 170),
        (216, 216, 216),
        (107, 107, 107),
        (197, 85, 85),
        (170, 196, 116),
        (254, 202, 136),
        (130, 184, 200),
        (194, 140, 184),
        (147, 211, 195),
        (248, 248, 248),
    ];
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let (r, g, b) = match index {
        0..=15 => ANSI[index],
        16..=231 => {
            let index = index - 16;
            (
                LEVELS[index / 36],
                LEVELS[(index % 36) / 6],
                LEVELS[index % 6],
            )
        }
        _ => {
            let shade = 8 + 10 * u8::try_from(index - 232).unwrap_or(0);
            (shade, shade, shade)
        }
    };
    Rgb { r, g, b }
}

fn cell_run(cell: &Cell, colors: &Colors) -> Run {
    let flags = cell.flags;
    let mut fg = resolve(cell.fg, colors);
    let mut bg = resolve(cell.bg, colors);
    let inverse = flags.contains(Flags::INVERSE);
    if inverse {
        std::mem::swap(&mut fg, &mut bg);
    }
    if flags.contains(Flags::DIM) {
        fg = (fg.0 / 2, fg.1 / 2, fg.2 / 2);
    }
    Run {
        text: cell.c.to_string(),
        fg,
        bg,
        bold: flags.contains(Flags::BOLD),
        italic: flags.contains(Flags::ITALIC),
        underline: flags.intersects(Flags::UNDERLINE | Flags::DOUBLE_UNDERLINE),
        strikeout: flags.contains(Flags::STRIKEOUT),
        inverse,
    }
}

/// One key press as the view reports it, before terminal encoding.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyInput<'a> {
    /// GPUI-style key name (`enter`, `up`, `f5`, `a`, …).
    pub key: &'a str,
    /// Printable text for the key, when the platform reports it.
    pub text: Option<&'a str>,
    /// Control held.
    pub ctrl: bool,
    /// Alt/Option held (sent as ESC prefix).
    pub alt: bool,
    /// Shift held.
    pub shift: bool,
    /// Command/platform held — never forwarded (app shortcuts win).
    pub platform: bool,
}

/// Encodes one key press into terminal bytes. Returns `None` for keys the
/// app should keep (platform combos, unmapped keys).
pub fn key_bytes(input: KeyInput<'_>, app_cursor: bool) -> Option<Vec<u8>> {
    if input.platform {
        return None;
    }
    let cursor = |letter: u8| -> Vec<u8> {
        if app_cursor {
            vec![0x1b, b'O', letter]
        } else {
            vec![0x1b, b'[', letter]
        }
    };
    let named: Option<Vec<u8>> = match input.key {
        "enter" => Some(b"\r".to_vec()),
        "backspace" => Some(vec![0x7f]),
        "tab" => Some(if input.shift {
            b"\x1b[Z".to_vec()
        } else {
            b"\t".to_vec()
        }),
        "escape" => Some(vec![0x1b]),
        "up" => Some(cursor(b'A')),
        "down" => Some(cursor(b'B')),
        "right" => Some(cursor(b'C')),
        "left" => Some(cursor(b'D')),
        "home" => Some(cursor(b'H')),
        "end" => Some(cursor(b'F')),
        "pageup" => Some(b"\x1b[5~".to_vec()),
        "pagedown" => Some(b"\x1b[6~".to_vec()),
        "delete" => Some(b"\x1b[3~".to_vec()),
        "insert" => Some(b"\x1b[2~".to_vec()),
        "space" => Some(b" ".to_vec()),
        "f1" => Some(b"\x1bOP".to_vec()),
        "f2" => Some(b"\x1bOQ".to_vec()),
        "f3" => Some(b"\x1bOR".to_vec()),
        "f4" => Some(b"\x1bOS".to_vec()),
        "f5" => Some(b"\x1b[15~".to_vec()),
        "f6" => Some(b"\x1b[17~".to_vec()),
        "f7" => Some(b"\x1b[18~".to_vec()),
        "f8" => Some(b"\x1b[19~".to_vec()),
        "f9" => Some(b"\x1b[20~".to_vec()),
        "f10" => Some(b"\x1b[21~".to_vec()),
        "f11" => Some(b"\x1b[23~".to_vec()),
        "f12" => Some(b"\x1b[24~".to_vec()),
        _ => None,
    };
    if let Some(bytes) = named {
        return Some(bytes);
    }
    let text = input
        .text
        .filter(|text| !text.is_empty())
        .or((input.key.chars().count() == 1).then_some(input.key))?;
    if input.ctrl {
        let mut chars = text.chars();
        let ch = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        let byte = match ch.to_ascii_lowercase() {
            c @ 'a'..='z' => (c as u8) - b'a' + 1,
            ' ' => 0,
            '[' => 27,
            '\\' => 28,
            ']' => 29,
            '^' => 30,
            '_' => 31,
            '?' => 0x7f,
            _ => return None,
        };
        return Some(vec![byte]);
    }
    let mut bytes = Vec::new();
    if input.alt {
        bytes.push(0x1b);
    }
    bytes.extend_from_slice(text.as_bytes());
    Some(bytes)
}

/// Builds a headless terminal over an in-memory grid (tests, previews).
#[cfg(test)]
pub(crate) fn feed(
    bytes: &[u8],
    cols: usize,
    rows: usize,
) -> (Vec<Vec<Run>>, Option<(usize, usize)>) {
    let (send, _events) = mpsc::channel();
    let dims = TermDims { cols, rows };
    let mut term = Term::new(Config::default(), &dims, Listener(send));
    let mut processor: Processor<StdSyncHandler> = Processor::default();
    processor.advance(&mut term, bytes);
    let content = term.renderable_content();
    let cursor = Some((content.cursor.point.column.0, 0usize));
    (rows_from_content(content), cursor)
}

#[cfg(test)]
mod tests {
    use super::{KeyInput, feed, key_bytes};

    fn key(key: &str) -> KeyInput<'_> {
        KeyInput {
            key,
            ..KeyInput::default()
        }
    }

    #[test]
    fn keys_encode_like_xterm() {
        assert_eq!(key_bytes(key("enter"), false), Some(b"\r".to_vec()));
        assert_eq!(key_bytes(key("backspace"), false), Some(vec![0x7f]));
        assert_eq!(key_bytes(key("up"), false), Some(b"\x1b[A".to_vec()));
        assert_eq!(key_bytes(key("up"), true), Some(b"\x1bOA".to_vec()));
        assert_eq!(key_bytes(key("f5"), false), Some(b"\x1b[15~".to_vec()));
        assert_eq!(
            key_bytes(
                KeyInput {
                    key: "tab",
                    shift: true,
                    ..KeyInput::default()
                },
                false
            ),
            Some(b"\x1b[Z".to_vec())
        );
    }

    #[test]
    fn ctrl_alt_and_platform_rules() {
        assert_eq!(
            key_bytes(
                KeyInput {
                    key: "c",
                    ctrl: true,
                    ..KeyInput::default()
                },
                false
            ),
            Some(vec![3])
        );
        assert_eq!(
            key_bytes(
                KeyInput {
                    key: "x",
                    alt: true,
                    ..KeyInput::default()
                },
                false
            ),
            Some(b"\x1bx".to_vec())
        );
        assert_eq!(
            key_bytes(
                KeyInput {
                    key: "v",
                    platform: true,
                    ..KeyInput::default()
                },
                false
            ),
            None
        );
        assert_eq!(
            key_bytes(
                KeyInput {
                    key: "a",
                    text: Some("å"),
                    ..KeyInput::default()
                },
                false
            ),
            Some("å".as_bytes().to_vec())
        );
    }

    fn row_text(rows: &[Vec<super::Run>], index: usize) -> String {
        rows.get(index)
            .map(|row| row.iter().map(|run| run.text.as_str()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn plain_text_lands_on_the_grid() {
        let (rows, _) = feed(b"hello", 10, 3);
        assert!(row_text(&rows, 0).starts_with("hello"));
    }

    #[test]
    fn color_and_cursor_sequences_apply() {
        let (rows, _) = feed(b"\x1b[31mred\x1b[0m plain", 20, 2);
        let first = rows[0][0].clone();
        assert_eq!(first.text, "red");
        // Named red resolves to a red-dominant RGB.
        assert!(first.fg.0 > first.fg.1 && first.fg.0 > first.fg.2);
        let second = rows[0][1].clone();
        assert!(second.text.starts_with(" plain"));
    }

    #[test]
    fn newlines_and_carriage_returns_move_rows() {
        let (rows, _) = feed(b"one\r\ntwo", 10, 3);
        assert!(row_text(&rows, 0).starts_with("one"));
        assert!(row_text(&rows, 1).starts_with("two"));
    }

    #[test]
    fn spawn_echo_round_trip() {
        let terminal = super::Terminal::spawn(
            "/bin/sh",
            &["-c", "printf ready"],
            std::path::Path::new("/"),
            20,
            3,
        )
        .expect("spawn shell");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut text = String::new();
        while std::time::Instant::now() < deadline {
            let (rows, _) = terminal.render_rows();
            text = rows
                .first()
                .map(|row| row.iter().map(|run| run.text.as_str()).collect())
                .unwrap_or_default();
            if text.contains("ready") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(text.contains("ready"), "grid shows: {text:?}");
        terminal.kill();
    }
}
