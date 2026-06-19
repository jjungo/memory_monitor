//! Application state: the current memory snapshot plus per-byte change tracking.

use crate::reader::MemReader;
use anyhow::Result;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}

pub struct Config {
    pub addr: u32,
    pub len: usize,
    pub bytes_per_row: usize,
    pub word_size: usize, // 1, 2 or 4
    pub endian: Endian,
    pub refresh: Duration,
    pub highlight: Duration,
}

pub struct App {
    pub cfg: Config,
    /// Latest snapshot of the region.
    pub data: Vec<u8>,
    /// Whether a byte has ever been populated (first read shouldn't flash everything).
    seeded: bool,
    /// Per-byte timestamp of the last observed change.
    last_change: Vec<Option<Instant>>,
    /// Vertical scroll offset, in rows.
    pub scroll: usize,
    pub paused: bool,
    pub status: String,
    pub last_read_ok: bool,
    pub reads: u64,
    pub last_refresh: Instant,
    /// When true, the address-entry prompt is active.
    pub input_mode: bool,
    /// Buffer for the address-entry prompt.
    pub input: String,
    /// Forces a read on the next loop iteration (used after a jump).
    pub force_refresh: bool,
}

impl App {
    pub fn new(cfg: Config) -> Self {
        let len = cfg.len;
        App {
            cfg,
            data: vec![0u8; len],
            seeded: false,
            last_change: vec![None; len],
            scroll: 0,
            paused: false,
            status: "starting…".to_string(),
            last_read_ok: false,
            reads: 0,
            last_refresh: Instant::now(),
            input_mode: false,
            input: String::new(),
            force_refresh: false,
        }
    }

    pub fn total_rows(&self) -> usize {
        self.cfg.len.div_ceil(self.cfg.bytes_per_row)
    }

    /// Point the monitor at a new base address, discarding stale change state.
    pub fn goto(&mut self, addr: u32) {
        self.cfg.addr = addr;
        self.data = vec![0u8; self.cfg.len];
        self.last_change = vec![None; self.cfg.len];
        self.seeded = false;
        self.scroll = 0;
        self.force_refresh = true;
    }

    /// Pull a fresh snapshot and update per-byte change timestamps.
    pub fn refresh(&mut self, reader: &mut dyn MemReader, now: Instant) -> Result<()> {
        let mut buf = vec![0u8; self.cfg.len];
        match reader.read(self.cfg.addr, &mut buf) {
            Ok(()) => {
                if self.seeded {
                    for i in 0..self.cfg.len {
                        if buf[i] != self.data[i] {
                            self.last_change[i] = Some(now);
                        }
                    }
                }
                self.data = buf;
                self.seeded = true;
                self.last_read_ok = true;
                self.reads += 1;
                self.status = reader.describe();
            }
            Err(e) => {
                self.last_read_ok = false;
                self.status = format!("read error: {e}");
            }
        }
        self.last_refresh = now;
        Ok(())
    }

    /// Is byte `i` currently within its highlight window?
    pub fn is_hot(&self, i: usize, now: Instant) -> bool {
        match self.last_change.get(i).and_then(|o| *o) {
            Some(t) => now.duration_since(t) < self.cfg.highlight,
            None => false,
        }
    }

    pub fn scroll_by(&mut self, delta: isize, viewport_rows: usize) {
        let max = self.total_rows().saturating_sub(viewport_rows);
        let next = self.scroll as isize + delta;
        self.scroll = next.clamp(0, max as isize) as usize;
    }

    pub fn scroll_to_bottom(&mut self, viewport_rows: usize) {
        self.scroll = self.total_rows().saturating_sub(viewport_rows);
    }
}
