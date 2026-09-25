# Instrument profiles

`instrument-viewer` can drive new instruments without any Rust code: drop a
YAML file here (or in `~/.config/instrument-viewer/profiles/`) that describes
the instrument's `*IDN?` pattern, capabilities, and command templates.

Profiles are consulted at startup, **before** the built-in hand-written
backends, so a profile whose `idn_matches` hits wins. Files must end in
`.yaml` or `.yml`.

## Quick start

Copy [`example.yaml`](./example.yaml), change `idn_matches` to a substring of
your instrument's `*IDN?` reply, and adjust the command templates. That is
enough for a Tek-command-set oscilloscope.

## Template language

Command templates support three placeholders:

| Placeholder | Meaning |
| :--- | :--- |
| `{n}` | 1-based channel number (`CH{n}` → `CH1`) |
| `{src}` | Trigger source (`TRIGGER:A:LEVEL:{src}?` → `TRIGGER:A:LEVEL:CH1?`) |
| `{v}` | The value being written |

A setting is `{ query: ..., write: ..., parse: ... }`. `query` and `write` are
optional (write-only settings like `autoset` have no query; read-only settings
have no write). `parse` is how the query reply is read:

| `parse` | Meaning |
| :--- | :--- |
| `f64` | Decimal number (default when omitted) |
| `bool` | `0`/`1`, `ON`/`OFF`, `RUN`/`STOP` |
| `character` | Enum or string, uppercased |

## Top-level fields

| Field | Required | Meaning |
| :--- | :--- | :--- |
| `idn_matches` | yes | Substrings matched case-insensitively against `*IDN?` |
| `name` | yes | Display name in the GUI |
| `driver` | no | Optional hand-written driver for deep quirks (see below) |
| `preamble` | no | Commands re-sent on every session resync |
| `capabilities` | no | What the GUI offers (see below) |
| `commands` | no | The command template table (see below) |
| `waveform` | no | Scope waveform transfer format |

### `driver`

A profile is normally served entirely by the generic YAML engine. When an
instrument has reply formats or transfers too odd to express as templates, the
profile can name one of the hand-written drivers — `tek`, `rigol`, `ds1000z`,
`dm3058`, `dsa800`, `sds`, `siglent`, `afg`, or `keysight` — and every
behavior method delegates to it
while the profile remains the catalog entry. The bundled profiles use this for
the quirkiest instruments (Rigol's chunked RAW transfer, the SDS `WF? DAT2`
scheme, supply pairing); over time the drivers shrink as the generic engine
grows.

## Bundled device profiles

Every supported instrument family has a profile in this directory:

| File | Device | `driver` |
| :--- | :--- | :--- |
| `tek-mdo3000.yaml` | Tektronix MDO3000 | none — fully YAML |
| `rigol-mso1000z.yaml` | Rigol DS1000Z / MSO1000Z | `ds1000z` |
| `rigol-dho900.yaml` | Rigol DHO900 | `rigol` |
| `rigol-dm3058.yaml` | Rigol DM3058/DM3058E multimeter | `dm3058` |
| `rigol-dsa800.yaml` | Rigol DSA800 spectrum analyzer | `dsa800` |
| `siglent-sdg1000x.yaml` | Siglent SDG1000X | `siglent` |
| `keysight-e36200.yaml` | Keysight E36200/E36300 supplies | `keysight` |
| `tek-afg3000.yaml` | Tektronix AFG3000 | `afg` |
| `siglent-sds1000x-e.yaml` | Siglent SDS1000X-E | `sds` |
| `example.yaml` | Example Tek-like scope | none — copy me |


## `capabilities`

Defaults are provided for every key, so list only what you want to change.

```yaml
capabilities:
  kind: oscilloscope        # oscilloscope | generator | supply
  channel_count: 4
  channel_couplings: [DC, AC]
  terminations:
    - { label: "1 MΩ", value: 1000000.0 }
  termination_writable: false
  bandwidths:
    - { label: "Full (100 MHz)", value: 100000000.0 }
  record_lengths: [1000, 10000, 100000]
  trigger_modes: [AUTO, NORMAL]
  trigger_slopes: [RISE, FALL]
  trigger_couplings: [DC, AC]
  acquisition_modes: [SAMPLE, AVERAGE]
  stop_after: [RUNSTOP, SEQUENCE]
  wave_types: [SINE, SQUARE]   # generators only
  output_pairs: []             # dual supplies only: OFF/PARALLEL/SERIES
  channel_hint: "shown under the channel panel"
  horizontal_hint: "shown under the timebase panel"
  acquisition_hint: "shown under the acquisition panel"
```

## `commands`

Every key is optional; missing keys make that control hidden or read-only.
Channel-scoped templates use `{n}`; trigger level uses `{src}`; everything
else is global.

| Key | Scope | Notes |
| :--- | :--- | :--- |
| `channel_enabled` | channel | display/output enable |
| `scale` | channel | V/div, Vpp, or supply volts |
| `position` | channel | divisions |
| `offset` | channel | volts, or supply current limit |
| `coupling` | channel | DC/AC/GND |
| `termination_ohms` | channel | input impedance |
| `bandwidth_hz` | channel | bandwidth limit |
| `probe_gain` | channel | probe transfer ratio |
| `probe_type` | channel | detected probe name |
| `wave_type` | channel | generator wave shape |
| `frequency_hz` | channel | generator frequency |
| `horizontal_scale` | global | seconds/div |
| `horizontal_position` | global | percent |
| `record_length` | global | points |
| `trigger_mode` | global | AUTO/NORMAL |
| `trigger_source` | global | channel name |
| `trigger_slope` | global | RISE/FALL |
| `trigger_coupling` | global | DC/AC/... |
| `trigger_level` | source | `{src}` template |
| `acquisition_mode` | global | SAMPLE/AVERAGE/... |
| `stop_after` | global | RUNSTOP/SEQUENCE |
| `running` | global | bool |
| `autoset` | global | write-only |

## `waveform`

Scopes only. The sample decoder is shared; the preamble parser is selected by
kind.

```yaml
waveform:
  encoding: i16be   # i16be | u16le | i8
  preamble: tek     # tek | rigol | sds
```

`preamble: tek` supports the full generic fetch path today
(`DATA:ENC RIBINARY` + `WFMOutpre?` + `CURVE?`). `rigol` and `sds` decode the
samples correctly but their transfer control flow is still in the hand-written
backends; profile-driven Rigol/SDS fetches are the next increment.

## Loading order

1. `~/.config/instrument-viewer/profiles/*.yaml` (user profiles)
2. `profiles/*.yaml` shipped with the app (built in)

The first profile whose `idn_matches` appears in the `*IDN?` reply wins; if no
profile matches, the built-in hand-written backends (Tek, Rigol, Siglent,
Keysight, AFG, SDS) are used as before.
