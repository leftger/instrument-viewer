# mdo-viewer

Rust GUI that pulls analog traces from a Tektronix **MDO3000 / MDO3024** over SCPI TCP and plots them.

Verified against `TEKTRONIX,MDO3024,B020857,CF:91.1CT FV:v1.30` at 169.254.6.252.

## Scope setup

1. Ethernet to the Mac. A direct cable is fine — both ends self-assign link-local
   `169.254.x.x` addresses. There is **no DHCP server** on that cable.
2. On the scope, **Ethernet & LXI → LAN Settings**:
   - **DHCP: Off** (otherwise LXI shows *LAN fault / Unable to renew DHCP lease*
     and the socket server drops in and out)
   - **Auto IP: On**, or a manual `169.254.x.x` / `255.255.0.0`
   - After changing this, use **Reset LAN** / **Test Connection** and re-read the IP
3. **Utility → Utility Page → I/O → Socket Server**
   - **Enabled**
   - Protocol **None** (Terminal mode prints a human help banner that desyncs a parser)
   - Port **4000**
4. Read the IP under **Ethernet & LXI → LAN Settings** (it can change after a LAN reset).

Check it before launching the GUI:

```bash
ping -c 3 169.254.6.252
nc -z -v 169.254.6.252 4000
```

## Run

```bash
cargo run --release
```

**Connect**, then **Fetch**, tick **Auto**, or **Sequence** (arm `STOPAFTER SEQUENCE`,
wait for the acquisition to complete, then pull the curve). **CSV** / **JSON** / **PNG**
save the traces already on the plot (JSON includes measurements and a settings
snapshot; **Wide** writes `t,CH1,CH2,…`). **Cursors**: left-click sets A, right-click
or Shift-click sets B; the bar under the plot shows Δt and 1/Δt.

The **View** row controls the plot:

- **Autoscale** refits both axes on every capture. Any manual zoom, scroll, or drag
  switches it off so the view stops jumping; **Fit now** is a one-shot refit.
- **Zoom axes X / Y** choose which axes zoom. Untick **X** to zoom vertically only —
  a Mac trackpad pinch is uniform, so this is how you get vertical-only zoom.
- **−** / **+** zoom the enabled axes; **−Y** / **+Y** always zoom vertically.
- **Scroll zooms** (default on) makes two-finger scroll zoom the enabled axes.
  Turn it off to pan with scroll instead.
- Drag pans, right-drag is a box zoom, and double-click resets.

Host, port, Auto interval, Wide CSV, and Scroll-zooms are remembered in
`~/.config/mdo-viewer/prefs`. `--host` / `--port` on the command line override the
saved address.

Auto captures every 2 seconds by default; the spinner next to it sets the interval
(0.5–30 s). The slow default is deliberate — see the quirks section. Auto switches
itself off on any error rather than retrying into a struggling instrument.

The left panel reads the current front-panel state and controls:

- CH1–CH4 enable, volts/div, position, offset, coupling, 1 MΩ/50 Ω input,
  passive-probe attenuation, and bandwidth
- time/div, horizontal position, and record length
- edge-trigger mode, source, slope, coupling, and level
- acquisition mode, continuous/single-sequence behavior, and run/stop
- Autoset and an advanced raw SCPI query/write console

Each Apply operation is read back from the instrument so values coerced by the
scope are reflected in the GUI. Physical front-panel changes can be imported with
**Refresh**. **Demo** plots a synthetic sine with no instrument attached.

> **50 Ω caution:** only select 50 Ω input termination when the source voltage is
> safe for the scope's internal terminator. Unlike a passive probe setting, this
> physically changes the input load.

Headless checks, useful for isolating app bugs from instrument bugs:

```bash
# Raw session: connect once, fetch N times. Safe to run.
cargo run --release --example probe -- 169.254.6.252:4000 CH1 40

# Drives the real GUI worker (connect, read config, fetch) without the window.
cargo run --release -- selftest --cycles 20

# Reconnects every cycle. This WILL wedge the instrument; see the quirks
# section. Only run it to demonstrate that failure mode.
cargo run --release -- selftest --cycles 25 --reconnect
```

## CLI control

With no subcommand, `mdo-viewer` launches the GUI. All CLI commands accept global
`--host` and `--port` options:

```bash
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
probe attenuation is converted to SCPI gain (`10x` → `PROBE:GAIN 0.1`).

## Protocol

No NI-VISA or TekVISA. Plain TCP client:

```
*CLS
HEADER OFF
DATA:SOURCE CH1
DATA:ENC RIBINARY
DATA:WIDTH 2
DATA:START 1
DATA:STOP <HORizontal:RECOrdlength?>
WFMOutpre?      # all scaling factors in one round trip
CURVE?          # IEEE 488.2 definite-length binary block
```

Scaling, with 16-bit big-endian samples:

- `t = XZERO + XINCR * i`
- `V = YZERO + YMULT * (raw - YOFF)`

A full 10,000-point channel takes about 110 ms end to end.

## Instrument quirks this works around

These are properties of the scope, not the app. They are the reason the code is
shaped the way it is.

**The output queue survives a TCP disconnect.** If you close a connection with an
unread response still queued, the scope hands that stale response to the *next*
connection. Every later reply then arrives one query behind, and the binary block
reader blocks hunting for a `#` that never comes. The session is long-lived, and
`resync()` clears the queue on connect and after any mid-transfer error.

**Repeated connect/disconnect wedges the instrument. This is the big one.**

Measured with `selftest --reconnect`: a connect → read-config → fetch → disconnect
cycle survives roughly 5–10 iterations, then the scope stops answering entirely.
TCP still accepts on port 4000, but nothing replies, including `*IDN?`.

Severity scales with how much churn it took:

- Mild churn: recovers on its own in roughly 30–60 seconds, *if left alone*.
  Polling it during that window appears to prevent recovery.
- Heavy churn: does not recover. Observed still dead after 3+ minutes of silence.
  At that point even VXI-11 (port 111) is affected — `create_link`,
  `device_clear` and `device_write` all succeed, but `device_read` returns
  error 15 (timeout), so the SCPI parser itself is hung rather than just the
  socket server. Only the front panel gets it back: toggle
  **Utility → I/O → Socket Server** off and on, or power cycle.

Draining the output queue before closing does *not* help; the churn itself is the
problem. So the app holds **one** connection for its whole lifetime, the Connect
and Disconnect buttons are disabled while an operation is in flight (queued
duplicate clicks were the main way to trigger this), and after a wedge is detected
the Connect button becomes a 30-second countdown instead of letting you hammer it.

Contrast: 40 consecutive fetches on a *single* connection run flawlessly at ~60 ms
each. Steady-state operation is not the problem. Reconnecting is.

If you see `IDN failed: timeout waiting for instrument response`, that is this bug.
The app clears the queue and retries once before reporting it. Wait out the
countdown; if two attempts fail, toggle the Socket Server on the front panel.

**`WFMOutpre?` field count is not stable.** This firmware returns 22 fields, with
model-specific entries both before (`PT_ORDER`) and after (`TIM;ANALOG;…`) the
scaling values, so neither head- nor tail-relative indexing is safe. The parser
anchors on the three quoted fields, which are always WFID, XUNIT and YUNIT.

**Querying scaling factors individually is slow.** Each `WFMOutpre:<field>?` costs
about 25 ms, so seven of them dominated the transfer. The combined `WFMOutpre?` is
one ~44 ms round trip.

**Out-of-range `DATA:STOP` is silently fatal.** `DATA:STOP 2000000` against a
10,000-point record stops the scope responding rather than clamping. Always set it
from `HORizontal:RECOrdlength?`.

## Not implemented

USB (USBTMC), digital-channel setup, non-edge triggers, and RF/spectrum controls.
Use the rear LAN port.
