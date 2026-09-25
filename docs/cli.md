# CLI control

With no subcommand, `instrument-viewer` launches the GUI. All CLI commands accept global
`--host` and `--port` options:

```bash
# Find link-local instruments via mDNS LXI and ARP neighbors
cargo run --release -- discover

# Read every supported setting
cargo run --release -- get

# Channel/probe setup; SI suffixes are accepted
cargo run --release -- channel 1 \
  --enabled true --probe 10 --scale 500mV --coupling dc \
  --termination one-meg --bandwidth 20MHz

# Horizontal setup
cargo run --release -- horizontal --scale 2us --position 50 --record-length 100k

# Edge trigger
cargo run --release -- trigger \
  --mode normal --source ch1 --slope rise --coupling dc --level 250mV

# Acquisition
cargo run --release -- acquisition \
  --mode average --stop-after sequence --running true

# Raw SCPI access
cargo run --release -- scpi query 'CH1:SCALE?'
cargo run --release -- scpi write 'CH1:SCALE 0.5'
cargo run --release -- autoset

# Waveform export (one fetch, then disconnect). Format follows --out suffix.
cargo run --release -- export --out capture.csv
cargo run --release -- export --wide --out capture.csv
cargo run --release -- export --format json --out capture.json --channels CH1,CH2
cargo run --release -- export --sequence --out shot.json
```

Run `cargo run --release -- <subcommand> --help` for the accepted values. Passive
probe attenuation is converted to SCPI gain (`10x` → `PROBE:GAIN 0.1`). `get`
prints settings in the shape of the detected instrument: scope channels and
timebase, generator outputs, supply setpoints, a meter function and reading, or
an analyzer's center/span and trace. `export` works for every class — a meter
exports its readings and a spectrum analyzer its sweep.

## Headless checks

Useful for isolating app bugs from instrument bugs:

```bash
# Raw session: connect once, fetch N times. Safe to run.
cargo run --release --example probe -- 169.254.6.252:4000 CH1 40

# Drives the real GUI worker (connect, read config, fetch) without the window.
cargo run --release -- selftest --cycles 20

# Reconnects every cycle. This WILL wedge the instrument; see
# protocol.md for the details. Only run it to demonstrate that failure mode.
cargo run --release -- selftest --cycles 25 --reconnect
```
