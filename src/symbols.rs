//! ELF symbol overlay: map live addresses back to firmware symbol names.
//!
//! Loads the `.symtab` from a firmware ELF and answers two questions the UI
//! needs while rendering a region:
//!   * which symbol *contains* a given address (for the title + continuation
//!     rows), and
//!   * which symbols *begin* within a given byte span (for the row gutter).
//!
//! This is the symbol-table tier. Full DWARF type decoding (struct field
//! names and typed values) would build on the same lookups via `gimli`.

use anyhow::{Context, Result};
use object::{Object, ObjectSymbol, SymbolKind};

/// One named symbol with a 32-bit target address and its byte size (0 if none).
pub struct Symbol {
    pub name: String,
    pub addr: u32,
    pub size: u32,
}

/// All data/function symbols from an ELF, sorted by address for fast lookup.
pub struct SymbolTable {
    /// Sorted by `(addr, size)`.
    syms: Vec<Symbol>,
    pub path: String,
}

impl SymbolTable {
    /// Parse the ELF at `path` and collect its data and function symbols.
    pub fn load(path: &str) -> Result<Self> {
        let data = std::fs::read(path).with_context(|| format!("reading ELF {path}"))?;
        let file = object::File::parse(&*data).with_context(|| format!("parsing ELF {path}"))?;

        let mut syms = Vec::new();
        for s in file.symbols() {
            // Keep variables (Data) and functions (Text); skip sections, files, etc.
            if !matches!(s.kind(), SymbolKind::Data | SymbolKind::Text) {
                continue;
            }
            let addr = s.address();
            if addr == 0 || addr > u32::MAX as u64 {
                continue;
            }
            let name = match s.name() {
                Ok(n) if !n.is_empty() => n.to_string(),
                _ => continue,
            };
            syms.push(Symbol { name, addr: addr as u32, size: s.size() as u32 });
        }

        syms.sort_by_key(|s| (s.addr, s.size));
        syms.dedup_by(|a, b| a.addr == b.addr && a.name == b.name);

        Ok(SymbolTable { syms, path: path.to_string() })
    }

    pub fn len(&self) -> usize {
        self.syms.len()
    }

    /// Address of the symbol named exactly `name`, if any.
    pub fn resolve(&self, name: &str) -> Option<u32> {
        self.syms.iter().find(|s| s.name == name).map(|s| s.addr)
    }

    /// Byte size of the (first) sized symbol named exactly `name`, if any.
    pub fn size_of(&self, name: &str) -> Option<u32> {
        self.syms.iter().find(|s| s.name == name && s.size > 0).map(|s| s.size)
    }

    /// Symbol whose `[addr, addr+size)` range covers `addr`, with the offset
    /// into it. Prefers the nearest enclosing symbol when ranges nest.
    pub fn containing(&self, addr: u32) -> Option<(&Symbol, u32)> {
        // Index just past the last symbol starting at or before `addr`.
        let upper = self.syms.partition_point(|s| s.addr <= addr);
        // Walk back over the closest few starts; the first that still covers
        // `addr` wins (this is the tightest enclosing symbol).
        for s in self.syms[..upper].iter().rev().take(64) {
            if s.addr == addr {
                return Some((s, 0));
            }
            if s.size > 0 && addr < s.addr.saturating_add(s.size) {
                return Some((s, addr - s.addr));
            }
        }
        None
    }

    /// All symbols whose start address falls within `[addr, addr+len)`.
    pub fn starting_in(&self, addr: u32, len: u32) -> Vec<&Symbol> {
        let end = addr.saturating_add(len);
        let start = self.syms.partition_point(|s| s.addr < addr);
        self.syms[start..].iter().take_while(|s| s.addr < end).collect()
    }
}
