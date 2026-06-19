//! Memory-read backends.
//!
//! `MemReader` is the abstraction the UI loop reads through. Two implementations:
//!   * `JLinkReader` — talks to a real target via SEGGER's `libjlinkarm.so` (FFI).
//!   * `MockReader`  — a synthetic region that mutates itself, for testing the TUI
//!                     and the change-highlight logic without hardware.

use anyhow::{anyhow, bail, Context, Result};
use std::ffi::{c_char, c_int, c_void, CStr, CString};

/// Anything that can fill a byte buffer from a target address.
pub trait MemReader {
    /// Read `buf.len()` bytes starting at `addr` into `buf`.
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()>;
    /// Short human-readable description of the connection (shown in the UI).
    fn describe(&self) -> String;
}

// ---------------------------------------------------------------------------
// J-Link FFI backend
// ---------------------------------------------------------------------------

// SEGGER JLINKARM.h, SWD = 1, JTAG = 0.
const JLINKARM_TIF_SWD: c_int = 1;

type OpenExFn = unsafe extern "C" fn(*const c_void, *const c_void) -> *const c_char;
type ExecCommandFn = unsafe extern "C" fn(*const c_char, *mut c_char, c_int) -> c_int;
type TifSelectFn = unsafe extern "C" fn(c_int) -> c_int;
type SetSpeedFn = unsafe extern "C" fn(u32);
type ConnectFn = unsafe extern "C" fn() -> c_int;
type ReadMemFn = unsafe extern "C" fn(u32, u32, *mut c_void) -> c_int;
type CloseFn = unsafe extern "C" fn();
type SelectSnFn = unsafe extern "C" fn(u32) -> c_int;
type IsConnectedFn = unsafe extern "C" fn() -> c_int;

pub struct JLinkReader {
    // The library must outlive the resolved symbols; keep it owned and dropped last.
    _lib: libloading::Library,
    read_mem: ReadMemFn,
    is_connected: IsConnectedFn,
    close: CloseFn,
    device: String,
    speed_khz: u32,
}

/// Default search paths for the J-Link shared library on Linux.
pub const DEFAULT_LIB_CANDIDATES: &[&str] = &[
    "libjlinkarm.so",
    "/opt/SEGGER/JLink/libjlinkarm.so",
    "/usr/lib/x86_64-linux-gnu/libjlinkarm.so",
];

impl JLinkReader {
    /// Open the library, select the device/interface and connect to the target.
    pub fn connect(
        lib_path: Option<&str>,
        device: &str,
        speed_khz: u32,
        serial: Option<u32>,
    ) -> Result<Self> {
        let lib = unsafe { open_lib(lib_path)? };

        // Resolve every symbol up front so a missing one fails loudly here.
        unsafe {
            let select_sn: libloading::Symbol<SelectSnFn> = lib
                .get(b"JLINKARM_EMU_SelectByUSBSN\0")
                .context("symbol JLINKARM_EMU_SelectByUSBSN")?;
            let open_ex: libloading::Symbol<OpenExFn> =
                lib.get(b"JLINKARM_OpenEx\0").context("symbol JLINKARM_OpenEx")?;
            let exec: libloading::Symbol<ExecCommandFn> = lib
                .get(b"JLINKARM_ExecCommand\0")
                .context("symbol JLINKARM_ExecCommand")?;
            let tif_select: libloading::Symbol<TifSelectFn> = lib
                .get(b"JLINKARM_TIF_Select\0")
                .context("symbol JLINKARM_TIF_Select")?;
            let set_speed: libloading::Symbol<SetSpeedFn> = lib
                .get(b"JLINKARM_SetSpeed\0")
                .context("symbol JLINKARM_SetSpeed")?;
            let connect: libloading::Symbol<ConnectFn> =
                lib.get(b"JLINKARM_Connect\0").context("symbol JLINKARM_Connect")?;
            let read_mem: libloading::Symbol<ReadMemFn> =
                lib.get(b"JLINKARM_ReadMem\0").context("symbol JLINKARM_ReadMem")?;
            let close: libloading::Symbol<CloseFn> =
                lib.get(b"JLINKARM_Close\0").context("symbol JLINKARM_Close")?;
            let is_connected: libloading::Symbol<IsConnectedFn> = lib
                .get(b"JLINKARM_IsConnected\0")
                .context("symbol JLINKARM_IsConnected")?;

            // Pin a specific probe by serial number, if requested.
            if let Some(sn) = serial {
                let rc = select_sn(sn);
                if rc < 0 {
                    bail!("JLINKARM_EMU_SelectByUSBSN({sn}) failed with rc={rc}");
                }
            }

            // OpenEx returns NULL on success, otherwise a static error string.
            let err = open_ex(std::ptr::null(), std::ptr::null());
            if !err.is_null() {
                let msg = CStr::from_ptr(err).to_string_lossy().into_owned();
                bail!("JLINKARM_OpenEx failed: {msg}");
            }

            exec_command(&exec, &format!("device = {device}"))?;

            let rc = tif_select(JLINKARM_TIF_SWD);
            // TIF_Select returns the previously selected interface; non-negative is fine.
            if rc < 0 {
                bail!("JLINKARM_TIF_Select(SWD) failed with rc={rc}");
            }

            set_speed(speed_khz);

            let rc = connect();
            if rc != 0 {
                bail!("JLINKARM_Connect failed with rc={rc} (check wiring/power/device name)");
            }

            // Detach lifetimes: keep the raw fn pointers, keep the library alive in the struct.
            let read_mem = *read_mem;
            let is_connected = *is_connected;
            let close = *close;

            Ok(JLinkReader {
                _lib: lib,
                read_mem,
                is_connected,
                close,
                device: device.to_string(),
                speed_khz,
            })
        }
    }
}

impl MemReader for JLinkReader {
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()> {
        let n = buf.len() as u32;
        // Background memory access over AHB-AP: works while the CPU runs (no halt).
        let rc = unsafe { (self.read_mem)(addr, n, buf.as_mut_ptr() as *mut c_void) };
        if rc < 0 {
            bail!("JLINKARM_ReadMem(0x{addr:08X}, {n}) failed with rc={rc}");
        }
        Ok(())
    }

    fn describe(&self) -> String {
        let link = if unsafe { (self.is_connected)() } != 0 {
            "connected"
        } else {
            "DISCONNECTED"
        };
        format!("J-Link {} @ {} kHz [{}]", self.device, self.speed_khz, link)
    }
}

impl Drop for JLinkReader {
    fn drop(&mut self) {
        unsafe { (self.close)() };
    }
}

unsafe fn open_lib(lib_path: Option<&str>) -> Result<libloading::Library> {
    if let Some(p) = lib_path {
        return libloading::Library::new(p)
            .with_context(|| format!("loading J-Link library from {p}"));
    }
    let mut last_err = None;
    for cand in DEFAULT_LIB_CANDIDATES {
        match libloading::Library::new(cand) {
            Ok(lib) => return Ok(lib),
            Err(e) => last_err = Some((cand, e)),
        }
    }
    match last_err {
        Some((c, e)) => Err(anyhow!(
            "could not load libjlinkarm.so (tried defaults; last: {c}: {e}). \
             Pass --lib /path/to/libjlinkarm.so"
        )),
        None => Err(anyhow!("no library candidates")),
    }
}

unsafe fn exec_command(exec: &ExecCommandFn, cmd: &str) -> Result<()> {
    let c = CString::new(cmd).unwrap();
    let mut errbuf = vec![0 as c_char; 256];
    exec(c.as_ptr(), errbuf.as_mut_ptr(), errbuf.len() as c_int);
    let msg = CStr::from_ptr(errbuf.as_ptr()).to_string_lossy();
    if !msg.is_empty() {
        bail!("J-Link command `{cmd}` reported: {msg}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Mock backend (no hardware)
// ---------------------------------------------------------------------------

/// Procedural synthetic memory for exercising the UI without hardware.
///
/// Every address has a stable base value derived from a hash, so the whole
/// 32-bit space is readable and `goto` works anywhere. A small overlay of
/// mutated words is layered on top and grown each read so highlights fire.
pub struct MockReader {
    /// Sparse mutations: word-aligned address -> overriding value.
    overlay: std::collections::HashMap<u32, u32>,
    rng: u64,
}

impl MockReader {
    pub fn new(base: u32, _len: usize) -> Self {
        MockReader {
            overlay: std::collections::HashMap::new(),
            rng: 0x9E3779B97F4A7C15 ^ (base as u64),
        }
    }

    fn next_rand(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    /// Stable base byte for any address (no overlay applied).
    fn base_byte(addr: u32) -> u8 {
        // splitmix-style finalizer over the address
        let mut z = (addr as u64).wrapping_add(0x9E3779B97F4A7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        (z ^ (z >> 31)) as u8
    }

    /// Value of one byte = base value, overridden by any overlay word.
    fn byte_at(&self, addr: u32) -> u8 {
        let word_addr = addr & !3;
        match self.overlay.get(&word_addr) {
            Some(v) => v.to_le_bytes()[(addr & 3) as usize],
            None => Self::base_byte(addr),
        }
    }
}

impl MemReader for MockReader {
    fn read(&mut self, addr: u32, buf: &mut [u8]) -> Result<()> {
        let len = buf.len() as u32;
        if len == 0 {
            return Ok(());
        }
        // Mutate a handful of random words within the visible window so highlights fire.
        let words = (len / 4).max(1);
        let n_changes = 1 + (self.next_rand() as usize % 4);
        for _ in 0..n_changes {
            let w = (self.next_rand() as u32) % words;
            let word_addr = (addr & !3).wrapping_add(w * 4);
            let val = self.next_rand() as u32;
            self.overlay.insert(word_addr, val);
        }

        for (i, b) in buf.iter_mut().enumerate() {
            *b = self.byte_at(addr.wrapping_add(i as u32));
        }
        Ok(())
    }

    fn describe(&self) -> String {
        format!("MOCK procedural memory, {} mutated words [no hardware]", self.overlay.len())
    }
}
