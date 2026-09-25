# instrument-viewer

<p align="center">
  <img src="assets/aztec_rustacean.png" alt="instrument-viewer" width="100%">
</p>

[![CI](https://github.com/leftger/instrument-viewer/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/leftger/instrument-viewer/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/leftger/instrument-viewer/branch/main/graph/badge.svg)](https://codecov.io/gh/leftger/instrument-viewer)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

Rust GUI for lab instruments over SCPI, on TCP or USB **USBTMC** (no NI-VISA).
It plots analog traces from oscilloscopes, previews generator waveforms, graphs
supply voltage/current, shows multimeter and DAQ readings as large digits, and
draws spectrum-analyzer sweeps in dBm. Supported families:

- **Oscilloscopes** — Tektronix MDO3000, Rigol DHO900 / DS1000Z / MSO1000Z,
  Siglent SDS1000X-E, Hantek HRDO2000
- **Generators** — Siglent SDG1032X, Tektronix AFG3000
- **Supplies** — Keysight E36200 / E36300 series, including dual-output pairing
- **Multimeters & DAQ** — Rigol DM3058, Hantek HDM3000, Hantek DAQ4000A
- **Spectrum analyzers** — Rigol DSA800, Siglent SSA / SVA / SHA

---

## Supported instruments

| Instrument | Role | SCPI port | Verified against |
| :--- | :--- | :---: | :--- |
| Tektronix MDO3000 | Oscilloscope | 4000 | MDO3024, firmware v1.30 |
| Rigol DHO900 | Oscilloscope | 5555 | DHO924S, firmware 00.01.05 |
| Rigol DS1000Z / MSO1000Z | Oscilloscope | 5555 | DS1000Z programming guide |
| Siglent SDS1000X-E | Oscilloscope | 5025 | — |
| Hantek HRDO2000 series | Oscilloscope | 5025 | 202606 programming guide |
| Siglent SDG1032X | Waveform generator | 5025 | — |
| Tektronix AFG3000 | Waveform generator | 4000 | — |
| Keysight E36231A | Single-output DC supply | 5025 | — |
| Keysight E36233A | Dual-output DC supply | 5025 | E36233A, firmware 1.1.1-1.0.3-1.01 |
| Keysight E3631A / E36234A | Command-compatible supplies | 5025 | — |
| Rigol DM3058 / DM3058E | Bench multimeter | 5555 | DM3058 programming guide |
| Hantek HDM3000 | Bench multimeter | 5025 | SCPI reference v1.02 |
| Hantek DAQ4000A | Scanning DAQ / multimeter | 5025 | V1.01 programming manual |
| Rigol DSA800 | Spectrum analyzer | 5555 | DSA800 programming guide |
| Siglent SSA / SVA / SHA | Spectrum analyzers | 5025 | **best-effort** — see below |

The backend is selected automatically from the `*IDN?` reply, so controls adapt
to the instrument. Every family has a [YAML profile](profiles/) in the registry;
instruments with reply formats or transfers a profile cannot express delegate to
a hand-written driver, which the profile names. Any instrument can also be added
as a YAML profile without Rust code — see
[Instrument profiles](profiles/README.md).

> **Siglent SSA/SVA/SHA caveat:** the manual available for these models is an
> IVI-C driver guide with no SCPI strings, so the `siglent_ssa` driver is built
> from the publicly documented SSA3000X command set. It is written defensively
> (every control query has a fallback and the trace parser accepts ASCII or
> `REAL,32` in either byte order), but verify a first connection against real
> hardware before trusting it.

---

## Quick start

<p align="center">
  <img src="assets/screenshot.png" alt="instrument-viewer demo waveform" width="100%">
</p>

```bash
cargo run --release
```

**Connect**, then **Fetch**, tick **Auto**, or use **Sequence**. **CSV** /
**JSON** / **PNG** export the plotted traces, and **Cursors** measure Δt and
1/Δt. No instrument at hand? **Demo** plots a synthetic sine. The screenshot
above is reproducible from the app itself:

```bash
cargo run --release -- --screenshot assets/screenshot.png
```

---

## Highlights

- Live plotting with min/max decimation, autoscale, stacked channel bands, zoom, pan, and box-zoom
- Automatic backend selection from `*IDN?`; unsupported settings are hidden or read-only
- Scopes, generators, supplies, multimeters, scan DAQs, and spectrum analyzers behind one GUI
- Discovery over mDNS LXI, ARP neighbors, and USBTMC (no NI-VISA)
- Full CLI: read/configure channels, timebase, trigger, acquisition, raw SCPI, and CSV/JSON export
- Headless `selftest` and raw `probe` example for isolating app bugs from instrument bugs
- Hardware-free test suite that drives a loopback mock instrument, with coverage on Codecov

---

## Documentation

| Guide | Covers |
| :--- | :--- |
| [Using the GUI](docs/gui.md) | Toolbar, plot and view controls, per-class instrument panels, preferences, rendering notes |
| [Instrument setup](docs/instrument-setup.md) | Per-instrument network setup, ports, discovery and connection |
| [CLI control](docs/cli.md) | All subcommands, export formats, headless checks |
| [Protocol & quirks](docs/protocol.md) | SCPI transfer and scaling details, meter and analyzer reads, vendor quirks |
| [Tests, coverage & releases](docs/testing.md) | Mock instrument harness, what Codecov measures, cutting a release |
| [Instrument profiles](profiles/README.md) | Adding instruments as YAML without Rust code |

---

## Not implemented

USB CDC-only gadgets (not USBTMC), digital-channel setup, non-edge trigger
types, strain/temperature-probe DAQ modes, and marker/limit maths on spectrum
analyzers. The Siglent SSA/SVA/SHA driver is best-effort until a real SCPI
manual replaces the IVI-C one.

---

## License

Dual-licensed under either of:

- **MIT License** ([`LICENSE-MIT`](./LICENSE-MIT))
- **Apache License, Version 2.0** ([`LICENSE-APACHE`](./LICENSE-APACHE))

at your option.
