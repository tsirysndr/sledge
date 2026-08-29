<p align="center">
  <img src="assets/logo.svg" alt="sledge" width="440">
</p>

<h1 align="center">sledge</h1>

A small Rust CLI that hammers on smart cards — inspect, read, and write **ACS
memory cards** and **ACOS smart cards** through a PC/SC reader (built and tested
with the **ACS ACR39U**). It detects the inserted card from its ATR and speaks
the right protocol for each: the ACR39U memory-card pseudo-APDUs for synchronous
SLE cards, and ISO 7816 for ACOS.

> **Card support at a glance:** SLE memory cards are fully supported and tested.
> ACOS support is present but **experimental** (detect and inspect work;
> read/write are best-effort and untested against real personalization).

```console
$ sledge detect
Reader: ACS ACR39U ICC Reader
ATR: 3B0492231091

Card: SLE (SLE5528 memory card, 1024 bytes)
```

## Features

- **Automatic card detection** — identifies the card from its ATR and reports
  whether it is an **SLE** memory card or an **ACOS** processor card.
- **Read** — dump card memory as decoded text or a raw hex view, with
  `--offset` / `--length` control.
- **Write** — write text into the card, with `0xFF` padding, read-back
  verification, and a dry-run mode by default.
- **Full inspect** — ATR, card type, presentation-error-counter state, and a
  complete hex dump saved to `sle5528.bin`.
- **Memory-card aware connection** — transparently connects synchronous memory
  cards over the RAW protocol (they cannot negotiate T=0/T=1) and processor
  cards over T=0/T=1.
- **Transient-error recovery** — automatically recovers from
  `SCARD_W_RESET_CARD`, the card reset macOS raises on the first APDU of a
  shared session.
- **Safety-first writes** — the security code must be supplied explicitly, the
  error counter is checked before writing, and the tool refuses to write to a
  locked card. It never submits a PIN automatically.

## Supported cards

| Card | Kind | Capacity | Status |
|------|------|----------|--------|
| Infineon **SLE5528** (SLE4428-compatible) | Synchronous memory | 1024 bytes | Read + write, tested |
| ACS **ACOS3 / ACOS3-32** | Microprocessor (filesystem) | 32 KB | Detect + inspect; read/write experimental |

Other SLE44xx/55xx memory cards use the same command family and may work with
adjusted ATR constants.

## Requirements

- Rust (edition 2024) and Cargo.
- A PC/SC reader such as the ACS ACR39U.
- A working PC/SC stack:
  - **Linux** — `pcscd` plus a CCID driver with ACS memory-card support
    (`libacsccid` / distribution `libacsccid1`).
  - **macOS** — see the driver note below. **This matters** for memory cards.

### macOS driver note (important for SLE cards)

Apple's bundled CCID driver enumerates the ACR39U and reads its ATR, but it
**cannot transmit** the memory-card pseudo-APDUs — every write/read of an SLE
card fails with `NotTransacted`, even though `pcsc_scan` "sees" the card. The
generic upstream CCID driver is worse: it cannot even power on the memory card.

The fix is ACS's own open-source CCID driver, **[acsccid]**, which carries the
ACR39U card-power-on and memory-card fixes. Build and install it into the
macOS override directory so it takes precedence over Apple's:

```bash
# deps
brew install automake libtool gettext pkg-config libusb

# build acsccid (see acsccid INSTALL for the macOS static-libusb setup)
./MacOSX/configure && make

# install into the override path and restart the smartcard stack
sudo make install    # -> /usr/local/libexec/SmartCardServices/drivers/
sudo killall -9 com.apple.ifdreader usbsmartcardreaderd
# then unplug and replug the reader
```

If a memory-card command returns `NotTransacted`, this driver is not active.

[acsccid]: https://github.com/acshk/acsccid

## Installation

```bash
git clone https://github.com/tsirysndr/sledge.git
cd sledge
cargo build --release
# binary at target/release/sledge
```

## Usage

```
sledge [--reader <INDEX>] <COMMAND>

Commands:
  detect    Print only the detected card type (SLE or ACOS) and its ATR
  inspect   Detect the card and print full info, dumping SLE memory to a file
  read      Read text from the card
  write     Write text to the card

Options:
  --reader <INDEX>   0-based index into the PC/SC reader list [default: 0]
```

### Detect

```bash
sledge detect
```

### Inspect

Prints the ATR, card type, error-counter state, and a full hex dump, and saves
the raw memory to `sle5528.bin`:

```bash
sledge inspect
```

### Read

```bash
# Decoded as text (trailing 0xFF / 0x00 padding trimmed)
sledge read --offset 64 --length 16

# Raw hex view
sledge read --offset 0 --length 32 --raw

# ACOS: select an EF first
sledge read --file FF04 --offset 0 --length 32
```

| Flag | Meaning |
|------|---------|
| `--offset <N>` | Start byte (default `0`) |
| `--length <N>` | Bytes to read (default: to end of memory) |
| `--raw` | Hex dump instead of decoded text |
| `--file <HEX>` | (ACOS) EF file ID to `SELECT` first |

### Write

Writes are a **dry run** unless you pass `--yes`:

```bash
# 1. Dry run — prints the plan, touches nothing
sledge write "hello card" --offset 64 --psc FFFF

# 2. Real write — presents the PSC, writes, verifies by read-back
sledge write "hello card" --offset 64 --psc FFFF --yes
```

| Flag | Meaning |
|------|---------|
| `--psc <HEX>` | (SLE) security code, e.g. `FFFF`. **Required** to unlock writes |
| `--offset <N>` | Start byte (default `32`, past the SLE protected header) |
| `--length <N>` | Pad the written region to N bytes with `0xFF` |
| `--file <HEX>` | (ACOS) EF file ID to `SELECT` first |
| `--yes` | Actually perform the write (otherwise dry run) |

#### ⚠️ The security code (PSC)

SLE memory is write-protected until you present the correct 2-byte security
code. **A wrong code decrements the card's error counter**, and once it reaches
`00` the card is **permanently write-locked** (reads still work). Only pass a
PSC you know is correct — the common factory default is `FFFF`, but a
personalized card will differ.

The `PRESENT_CODE` command answers `90 <EC>`, where `EC` is the error counter
after the attempt: `FF` means the code was accepted (and the counter is
restored); a lower value means it was wrong. `sledge` checks this, prints
the counter, and refuses to write when the card is locked.

Do not write at offset `0`: the first ~27 bytes hold the ATR / protected /
manufacturer area. The user zone starts higher up, which is why the write
default is offset `32`.

## Project layout

```
src/
├── main.rs           Entry point: parse args, connect, dispatch
├── cli.rs            clap command/argument definitions
├── card.rs           Connection, card-kind detection, transmit + reset recovery
├── sle.rs            SLE5528 memory-card commands
├── acos.rs           ACOS3 ISO 7816 file commands
├── util.rs           hexdump, hex parsing, text decoding
└── commands/         one file per subcommand
    ├── detect.rs
    ├── inspect.rs
    ├── read.rs
    └── write.rs
```

## How it works

- **Connection** — `SCardConnect` is tried with T=0/T=1 first; a synchronous
  memory card cannot negotiate these and returns `UNRESPONSIVE_CARD`, so the
  tool falls back to the RAW protocol. The card is then re-powered once to clear
  any state inherited from a previous session.
- **SLE memory cards** — driven with the ACR39U pseudo-APDUs: `FF A4` (select
  card type), `FF B0` (read), `FF D0` (write), `FF 20` (present code),
  `FF B1` (read error counter).
- **ACOS cards** — driven with ISO 7816 `SELECT FILE`, `READ BINARY`, and
  `UPDATE BINARY`. Because access conditions are fixed at personalization,
  these may be rejected depending on the card; no PIN is ever auto-submitted.

## Disclaimer

This tool talks directly to smart cards and can modify their contents. Writing
with an incorrect security code can permanently lock a card. Use it only on
cards you own and understand. Provided as-is, without warranty (see LICENSE).

## License

Released under the [MIT License](LICENSE).
