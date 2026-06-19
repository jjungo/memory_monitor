//! memory_monitor — TUI hexdump monitor for live target memory over a SEGGER J-Link.
//!
//! Reads a memory region from an nRF52840 (or any Cortex-M) via background AHB-AP
//! access (the CPU keeps running) and renders it as a refreshing hexdump. Bytes that
//! change between refreshes flash red for a configurable window.

mod app;
mod reader;
mod ui;

use anyhow::{bail, Context, Result};
use app::{App, Config, Endian};
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use reader::{JLinkReader, MemReader, MockReader};
use std::io::stdout;
use std::time::{Duration, Instant};

/// Parse a number with optional 0x / 0X prefix (hex) or plain decimal.
fn parse_u32(s: &str) -> Result<u32, String> {
    let s = s.trim();
    let v = if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(h, 16)
    } else {
        s.parse::<u32>()
    };
    v.map_err(|e| format!("invalid number `{s}`: {e}"))
}

fn parse_usize(s: &str) -> Result<usize, String> {
    parse_u32(s).map(|v| v as usize)
}

#[derive(Parser)]
#[command(
    name = "memory_monitor",
    about = "TUI hexdump monitor for live target memory over a SEGGER J-Link",
    long_about = None,
)]
struct Cli {
    /// Start address of the region to monitor (e.g. 0x2003F6C0).
    #[arg(short, long, value_parser = parse_u32)]
    addr: u32,

    /// Number of bytes to monitor (e.g. 0x140 or 320).
    #[arg(short, long, value_parser = parse_usize, default_value = "256")]
    len: usize,

    /// J-Link target device name.
    #[arg(short, long, default_value = "nRF52840_xxAA")]
    device: String,

    /// SWD interface speed in kHz.
    #[arg(short, long, default_value = "4000")]
    speed: u32,

    /// Refresh interval in milliseconds.
    #[arg(short, long, default_value = "200")]
    refresh: u64,

    /// Highlight duration for changed bytes, in milliseconds.
    #[arg(long, default_value = "500")]
    highlight: u64,

    /// Bytes per displayed row.
    #[arg(long, default_value = "16")]
    width: usize,

    /// Word grouping in bytes for the hex columns: 1, 2 or 4.
    #[arg(long, default_value = "4")]
    word: usize,

    /// Interpret multi-byte words as big-endian instead of little-endian.
    #[arg(long)]
    big_endian: bool,

    /// Pin a specific probe by its J-Link USB serial number.
    #[arg(long)]
    serial: Option<u32>,

    /// Path to libjlinkarm.so (auto-detected if omitted).
    #[arg(long)]
    lib: Option<String>,

    /// Run against a synthetic, self-mutating region instead of hardware.
    #[arg(long)]
    mock: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if !matches!(cli.word, 1 | 2 | 4) {
        bail!("--word must be 1, 2 or 4 (got {})", cli.word);
    }
    if cli.width == 0 || cli.width % cli.word != 0 {
        bail!("--width ({}) must be a non-zero multiple of --word ({})", cli.width, cli.word);
    }
    if cli.len == 0 {
        bail!("--len must be greater than 0");
    }

    let mut reader: Box<dyn MemReader> = if cli.mock {
        Box::new(MockReader::new(cli.addr, cli.len))
    } else {
        Box::new(
            JLinkReader::connect(cli.lib.as_deref(), &cli.device, cli.speed, cli.serial)
                .context("connecting to target via J-Link")?,
        )
    };

    let cfg = Config {
        addr: cli.addr,
        len: cli.len,
        bytes_per_row: cli.width,
        word_size: cli.word,
        endian: if cli.big_endian { Endian::Big } else { Endian::Little },
        refresh: Duration::from_millis(cli.refresh),
        highlight: Duration::from_millis(cli.highlight),
    };
    let mut app = App::new(cfg);

    run_tui(&mut app, reader.as_mut())
}

fn run_tui(app: &mut App, reader: &mut dyn MemReader) -> Result<()> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(out);
    let mut terminal = ratatui::Terminal::new(backend)?;

    let result = event_loop(app, reader, &mut terminal);

    // Always restore the terminal, even on error.
    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    result
}

fn event_loop<B: ratatui::backend::Backend>(
    app: &mut App,
    reader: &mut dyn MemReader,
    terminal: &mut ratatui::Terminal<B>,
) -> Result<()> {
    // Redraw fast enough to expire highlights crisply, regardless of refresh rate.
    let frame_interval = Duration::from_millis(50).min(app.cfg.refresh);

    // Prime an initial read so the first frame shows data without flashing.
    let now = Instant::now();
    app.refresh(reader, now)?;

    loop {
        let now = Instant::now();

        let due = now.duration_since(app.last_refresh) >= app.cfg.refresh;
        if app.force_refresh || (!app.paused && due) {
            app.force_refresh = false;
            app.refresh(reader, now)?;
        }

        let mut vp = 0usize;
        terminal.draw(|f| {
            vp = ui::viewport_rows(f.area().height);
            ui::draw(f, app, Instant::now());
        })?;

        // Wait for input up to the next frame deadline.
        let timeout = frame_interval;
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

                // Address-entry prompt swallows all other keys while active.
                if app.input_mode {
                    match key.code {
                        KeyCode::Esc => {
                            app.input_mode = false;
                            app.input.clear();
                        }
                        KeyCode::Enter => {
                            // Address field: interpret input as hex (0x prefix optional).
                            let s = app.input.trim();
                            let hex = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")).unwrap_or(s);
                            if let Ok(addr) = u32::from_str_radix(hex, 16) {
                                app.goto(addr);
                                app.input_mode = false;
                                app.input.clear();
                            }
                            // On a parse error, keep the prompt open so it can be fixed.
                        }
                        KeyCode::Backspace => {
                            app.input.pop();
                        }
                        KeyCode::Char('c') if ctrl => break,
                        KeyCode::Char(c) if c.is_ascii_hexdigit() || c == 'x' || c == 'X' => {
                            if app.input.len() < 10 {
                                app.input.push(c);
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Char('g') if ctrl => {
                        app.input_mode = true;
                        app.input.clear();
                    }
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('c') if ctrl => break,
                    KeyCode::Char(' ') => app.paused = !app.paused,
                    KeyCode::Char('+') | KeyCode::Char('=') => {
                        app.cfg.refresh = (app.cfg.refresh + Duration::from_millis(50))
                            .min(Duration::from_secs(10));
                    }
                    KeyCode::Char('-') | KeyCode::Char('_') => {
                        app.cfg.refresh = app
                            .cfg
                            .refresh
                            .checked_sub(Duration::from_millis(50))
                            .unwrap_or(Duration::from_millis(10))
                            .max(Duration::from_millis(10));
                    }
                    KeyCode::Up => app.scroll_by(-1, vp),
                    KeyCode::Down => app.scroll_by(1, vp),
                    KeyCode::PageUp => app.scroll_by(-(vp as isize), vp),
                    KeyCode::PageDown => app.scroll_by(vp as isize, vp),
                    KeyCode::Char('g') | KeyCode::Home => app.scroll = 0,
                    KeyCode::Char('G') | KeyCode::End => app.scroll_to_bottom(vp),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}
