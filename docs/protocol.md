# Protocol & instrument quirks

## Protocol

LAN uses a plain TCP SCPI client. USB uses USBTMC bulk transfers (not NI-VISA /
TekVISA). The backend is selected after `*IDN?`.

Tektronix transfer:

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

Rigol transfer:

```
:WAV:SOUR CHAN1
:WAV:FORM WORD
:WAV:MODE NORM|RAW
:WAV:PRE?
:WAV:DATA?
```

Rigol WORD samples are unsigned little-endian:

- `t = XORIGIN + XINCREMENT * (i - XREFERENCE)`
- `V = (raw - YORIGIN - YREFERENCE) * YINCREMENT`

`NORM` returns the 1,000 on-screen points while the scope is running. After a
single acquisition stops, `RAW` returns the full acquisition memory; long
records are transferred in windows.

## Tektronix instrument quirks this works around

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

## Rigol DHO900 notes

The DHO900 firmware accepts much of the Tektronix command vocabulary, but not
waveform transfer, channel display, or source-qualified trigger-level commands.
Those operations use Rigol-native SCPI through the Rigol backend.

`ACQUIRE:STOPAFTER SEQUENCE` is accepted and reads back successfully but does
not stop the DHO900. The Sequence button therefore uses native `:SING` and polls
`:TRIG:STAT?` for `STOP`.

The DHO900 inputs are fixed at 1 MΩ. Attempts to apply 50 Ω termination return a
clear unsupported-setting error. Its bandwidth control is an OFF/20 MHz limit,
which the GUI presents as full instrument bandwidth or 20 MHz.
