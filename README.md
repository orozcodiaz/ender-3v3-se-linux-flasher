# ender-flash

Linux CLI to **inspect a removable SD card** and **copy Ender 3 V3 SE firmware** (`.bin`) onto it.

## Why this order?

Best practice for a “is this card OK, then flash” tool is **drive-first**:

1. Select the mounted SD card (`↑` / `↓`, Enter)
2. Review contents + short stats (files/folders, occupied space, FS type, free MB / %)
3. Choose the firmware `.bin` (existence and extension checked)
4. Confirm a short destination name and copy (with `fsync`)

That fails fast if the wrong stick is inserted, and keeps the flash step deliberate.

## Requirements

- Linux with `lsblk` (util-linux)
- A mounted removable volume (USB SD reader, etc.)
- Rust 1.70+ to build from source

## Build

```bash
cargo build --release
# binary: target/release/ender-flash
```

Or run without installing:

```bash
cargo run
```

## Usage

Interactive wizard (default):

```bash
ender-flash
# same as:
ender-flash flash
```

Inspect only (no copy):

```bash
ender-flash inspect
```

Non-interactive / scripted:

```bash
ender-flash --drive /media/$USER/SDCARD --bin ./firmware.bin --as-name hfie93.bin --yes
```

| Flag | Meaning |
|------|---------|
| `--drive` | Mountpoint, device node, or `lsblk` name |
| `--bin` | Path to `.bin` firmware |
| `--as-name` | Short filename on the card (≤8 chars before `.bin`) |
| `--yes` | Skip confirmation prompts |

Interactive mode suggests a **new random** name each run (e.g. `hfie93.bin`) so the printer does not ignore a previously seen filename.

## Ender 3 V3 SE notes

- Flash via **SD card**, not a direct USB firmware upload.
- Prefer **FAT32**, often with **4096** allocation unit size.
- Put a **short** `.bin` name on the card (e.g. `hfie93.bin`); use a **new name** each attempt — bootloaders ignore names they already saw.
- Stock Creality firmware usually goes in the **root** of the card.
- Some **C14 / Klipper** flows use a folder named `STM32F4_UPDATE` — follow your board-specific guide.
- After copy: eject → printer off → insert motherboard SD slot → power on ~10–15s → power off → remove card.

## License

MIT
