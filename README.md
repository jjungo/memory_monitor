# memory_monitor

TUI hexdump monitor for a live target memory region over a SEGGER J-Link.
Reads via background AHB-AP access (the CPU keeps running — no halt), renders
the region as a refreshing hexdump in the SEGGER Memory-window layout, and
flashes changed bytes **red** for a configurable window.

## Build

```sh
cargo build --release
# binary at target/release/memory_monitor
```

## Run (hardware)

```sh
memory_monitor --addr 0x2003F6C0 --len 0x140 \
    --device nRF52840_xxAA --speed 4000 --refresh 200
```

The J-Link library (`libjlinkarm.so`) is auto-detected from the default SEGGER
install paths; override with `--lib /opt/SEGGER/JLink_V752d/libjlinkarm.so`.
Pin a specific probe with `--serial <usb-sn>`.

> `Connect` establishes a debug connection to the target. Background memory
> reads do not halt the CPU, but the initial connect can disturb a target
> depending on its debug state — connect when you're ready.

## Run (no hardware)

```sh
memory_monitor --mock --addr 0x2003F6C0 --len 0x140 --refresh 100
```

A synthetic, self-mutating region for exercising the TUI and the highlight logic.

## Symbol overlay (ELF)

Point the monitor at the target's ELF and it maps live addresses back to symbol
names: the title shows the symbol enclosing the base address, and each hexdump
row gets a right-hand gutter naming the symbols that begin on it (bright) or the
enclosing symbol with its offset (dim). Toggle the overlay with `s`.

```sh
memory_monitor --elf firmware.elf --addr 0x20006000 --len 0x80
```

With `--elf`, `--addr` (and the `Ctrl-G` go-to prompt) also accept **symbol
names** instead of numbers. When `--addr` names a sized symbol and `--len` is
omitted, the region defaults to that symbol's size:

```sh
memory_monitor --elf firmware.elf --addr g_state   # len = sizeof(g_state)
```

## Options

| flag | default | meaning |
|------|---------|---------|
| `--addr` | (required) | start address (`0x..` hex or decimal) or, with `--elf`, a symbol name |
| `--len` | `256` | bytes to monitor (defaults to the symbol size when `--addr` names a sized symbol) |
| `--elf` | — | ELF to load symbols from, enabling the symbol overlay and symbol-name addresses |
| `--device` | `nRF52840_xxAA` | J-Link device name |
| `--speed` | `4000` | SWD speed (kHz) |
| `--refresh` | `200` | refresh interval (ms) |
| `--highlight` | `500` | red-highlight duration for changed bytes (ms) |
| `--width` | `16` | bytes per row |
| `--word` | `4` | hex grouping: 1, 2 or 4 bytes (LE value, like the SEGGER viewer) |
| `--big-endian` | off | interpret words big-endian |
| `--serial` | — | pin probe by USB serial |
| `--lib` | auto | path to `libjlinkarm.so` |
| `--mock` | off | synthetic region, no hardware |

## Keys

| key | action |
|-----|--------|
| `q` / `Esc` / `Ctrl-C` | quit |
| `space` | pause / resume refresh |
| `s` | toggle the ELF symbol overlay (when `--elf` is given) |
| `+` / `-` | refresh interval ±50 ms |
| `↑` `↓` `PgUp` `PgDn` | scroll |
| `g` / `G` | top / bottom |
| `Ctrl-G` | go to address — type a hex address (`0x` optional), Enter to jump, Esc to cancel |
