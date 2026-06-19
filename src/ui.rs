//! Hexdump rendering, matching the SEGGER J-Link Memory window layout:
//! address column, word columns (little-endian grouped), then ASCII.

use crate::app::{App, Endian};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph},
};
use std::time::Instant;

const HOT: Color = Color::Red;
const ADDR_COL: Color = Color::DarkGray;
const ASCII_COL: Color = Color::Gray;
const HEX_COL: Color = Color::White;
const SYM_START: Color = Color::Cyan; // a symbol begins on this row
const SYM_CONT: Color = Color::DarkGray; // continuation of an enclosing symbol

/// Trim an over-long symbol name (mangled C++ names can be huge) for display.
fn short_name(name: &str) -> String {
    const MAX: usize = 40;
    if name.len() <= MAX {
        name.to_string()
    } else {
        format!("{}…", &name[..MAX - 1])
    }
}

/// Number of body rows the hex viewport can show for a given total height.
pub fn viewport_rows(area_height: u16) -> usize {
    // area minus borders (2) minus header line (1).
    (area_height as usize).saturating_sub(3)
}

pub fn draw(f: &mut Frame, app: &App, now: Instant) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1), Constraint::Length(1)])
        .split(f.area());

    draw_title(f, app, chunks[0]);
    draw_hex(f, app, now, chunks[1]);
    draw_footer(f, app, chunks[2]);
}

fn draw_title(f: &mut Frame, app: &App, area: Rect) {
    let c = &app.cfg;
    // Annotate the base address with the enclosing symbol, when known.
    let sym = if app.overlay {
        app.symbols
            .as_ref()
            .and_then(|t| t.containing(c.addr))
            .map(|(s, off)| {
                if off == 0 {
                    format!("  <{}>", short_name(&s.name))
                } else {
                    format!("  <{}+0x{:X}>", short_name(&s.name), off)
                }
            })
            .unwrap_or_default()
    } else {
        String::new()
    };
    let title = format!(
        " Memory @ 0x{:08X}{}  +0x{:X} bytes  •  {} ms refresh  •  {}{} ",
        c.addr,
        sym,
        c.len,
        c.refresh.as_millis(),
        app.status,
        if app.paused { "  [PAUSED]" } else { "" },
    );
    let style = if app.last_read_ok {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default().fg(Color::White).bg(Color::Red)
    };
    f.render_widget(
        Paragraph::new(title).style(style.add_modifier(Modifier::BOLD)),
        area,
    );
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    if app.input_mode {
        let line = Line::from(vec![
            Span::styled(
                if app.symbols.is_some() { " Go to address / symbol: " } else { " Go to address: " },
                Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{}\u{2588}", app.input),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  (Enter to jump, Esc to cancel)", Style::default().fg(Color::DarkGray)),
        ]);
        f.render_widget(Paragraph::new(line), area);
        return;
    }
    let sym_help = match app.symbols.as_ref() {
        Some(t) => format!(" │ s syms({}):{}", t.len(), if app.overlay { "on" } else { "off" }),
        None => String::new(),
    };
    let help = format!(
        " q quit │ space pause │ +/- refresh │ ↑/↓ PgUp/PgDn scroll │ g/G top/bottom │ ^G goto{} │ reads:{} ",
        sym_help, app.reads
    );
    f.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn draw_hex(f: &mut Frame, app: &App, now: Instant, area: Rect) {
    let c = &app.cfg;
    let block = Block::default().borders(Borders::ALL).title(" hexdump ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows_visible = inner.height.saturating_sub(1) as usize; // 1 line for the column header
    let bpr = c.bytes_per_row;

    let mut lines: Vec<Line> = Vec::with_capacity(rows_visible + 1);
    lines.push(header_line(c.bytes_per_row, c.word_size));

    let start_row = app.scroll;
    for row in start_row..(start_row + rows_visible) {
        let row_off = row * bpr;
        if row_off >= c.len {
            break;
        }
        lines.push(hex_line(app, now, row_off, bpr));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

fn header_line(bpr: usize, word: usize) -> Line<'static> {
    // Build the "offset within row" header, e.g. for word=4: 0        4        8 ...
    let mut s = String::from("          "); // under the 8-hex addr + 2 spaces
    let mut col = 0;
    while col < bpr {
        let label = format!("{:<width$}", format!("+{:X}", col), width = word * 2 + 1);
        s.push_str(&label);
        col += word;
    }
    Line::from(Span::styled(s, Style::default().fg(ADDR_COL).add_modifier(Modifier::DIM)))
}

fn hex_line<'a>(app: &App, now: Instant, row_off: usize, bpr: usize) -> Line<'a> {
    let c = &app.cfg;
    let mut spans: Vec<Span<'a>> = Vec::with_capacity(bpr + 4);

    // Address column.
    let addr = c.addr.wrapping_add(row_off as u32);
    spans.push(Span::styled(
        format!("{:08X}  ", addr),
        Style::default().fg(ADDR_COL),
    ));

    // Hex words.
    let mut col = 0;
    while col < bpr {
        let off = row_off + col;
        if off >= c.len {
            // pad missing cells so the ASCII column stays aligned
            spans.push(Span::raw(" ".repeat(c.word_size * 2 + 1)));
            col += c.word_size;
            continue;
        }
        let w = c.word_size.min(c.len - off);
        let bytes = &app.data[off..off + w];
        let hot = (off..off + w).any(|i| app.is_hot(i, now));
        let text = format_word(bytes, c.word_size, c.endian);
        let style = if hot {
            Style::default().fg(HOT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(HEX_COL)
        };
        spans.push(Span::styled(format!("{text} "), style));
        col += c.word_size;
    }

    spans.push(Span::raw(" "));

    // ASCII column — per-byte highlight.
    for col in 0..bpr {
        let off = row_off + col;
        if off >= c.len {
            break;
        }
        let b = app.data[off];
        let ch = if (0x20..0x7f).contains(&b) { b as char } else { '.' };
        let style = if app.is_hot(off, now) {
            Style::default().fg(HOT).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(ASCII_COL)
        };
        spans.push(Span::styled(ch.to_string(), style));
    }

    // Symbol gutter: name(s) starting on this row (bright), else the enclosing
    // symbol as a dim continuation marker.
    if app.overlay {
        if let Some(tbl) = app.symbols.as_ref() {
            let row_addr = c.addr.wrapping_add(row_off as u32);
            let row_len = (c.len - row_off).min(bpr) as u32;
            let starts = tbl.starting_in(row_addr, row_len);
            if !starts.is_empty() {
                let label = starts
                    .iter()
                    .map(|s| short_name(&s.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                spans.push(Span::raw("   "));
                spans.push(Span::styled(
                    label,
                    Style::default().fg(SYM_START).add_modifier(Modifier::BOLD),
                ));
            } else if let Some((s, off)) = tbl.containing(row_addr) {
                spans.push(Span::raw("   "));
                spans.push(Span::styled(
                    format!("{}+0x{:X}", short_name(&s.name), off),
                    Style::default().fg(SYM_CONT).add_modifier(Modifier::DIM),
                ));
            }
        }
    }

    Line::from(spans)
}

/// Format `word_size` bytes as the J-Link viewer does: a grouped value.
/// For multi-byte words the bytes are interpreted with the configured endianness.
fn format_word(bytes: &[u8], word_size: usize, endian: Endian) -> String {
    match word_size {
        1 => format!("{:02X}", bytes[0]),
        2 | 4 | 8 => {
            let mut v: u64 = 0;
            match endian {
                Endian::Little => {
                    for (i, b) in bytes.iter().enumerate() {
                        v |= (*b as u64) << (8 * i);
                    }
                }
                Endian::Big => {
                    for b in bytes {
                        v = (v << 8) | (*b as u64);
                    }
                }
            }
            format!("{:0width$X}", v, width = word_size * 2)
        }
        _ => bytes.iter().map(|b| format!("{:02X}", b)).collect(),
    }
}
