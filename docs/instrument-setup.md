# Instrument setup

## Tektronix MDO3000

Verified against a Tektronix MDO3024 (firmware v1.30).

1. Ethernet to the computer. A direct cable is fine — both ends self-assign link-local
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

## Rigol DHO900

Verified against a Rigol DHO924S (firmware 00.01.05).

Connect the scope to the same network as the computer (or use a direct Ethernet
connection), note its IP under the LAN settings, and use raw SCPI port **5555**.
The scope does not listen on the Tektronix default port 4000.

```bash
ping -c 3 <scope-ip>
nc -z -v <scope-ip> 5555
cargo run --release -- --host <scope-ip> --port 5555
```

## Siglent SDG1032X

The SDG is an **arbitrary waveform generator**, not a scope. LAN socket port is
**5025** (programming guide PG02_E05C). `*IDN?` looks like
`Siglent Technologies,SDG1032X,<serial>,<firmware>`. Scan will pick it up; Connect
selects a generator backend so the app does not send Tektronix commands.

Output on/off, wave type, frequency, amplitude (Vpp), offset, and 50 Ω/HiZ load
are set with `C1`/`C2` `OUTP` and `BSWV`. **Fetch** plots a preview of that
programmed wave (it does not digitise the BNCs). ARB mode is previewed as a sine.

```bash
ping -c 3 <sdg-ip>
nc -z -v <sdg-ip> 5025
cargo run --release -- --host <sdg-ip> --port 5025
```

## Keysight E36231A

The E36231A is a **single-output 30 V / 20 A / 200 W** DC supply, not a scope.
LAN socket port is **5025**. `*IDN?` looks like
`Keysight Technologies,E36231A,<serial>,<firmware>`. Scan will pick it up; Connect
selects a supply backend so the app does not send Tektronix commands.

Voltage, current limit, and output on/off use `VOLT`, `CURR`, and `OUTP`
(E36200 programming guide). **Fetch** reads one `MEAS:VOLT?` and `MEAS:CURR?`
sample. The window shows that reading in large digits. A second Fetch in the
same session plots voltage and current against seconds since the first reading.
The older
triple-output E3631A uses `APPL P6V|P25V|N25V` (same pattern as the
[E3631A Python driver](https://github.com/psmd-iberutaru/Keysight-E3631A-Python)).

```bash
ping -c 3 <psu-ip>
nc -z -v <psu-ip> 5025
cargo run --release -- --host <psu-ip> --port 5025
```

## Keysight E36233A

The E36233A is a **dual-output** autoranging supply. Each output is 30 V / 20 A
and can deliver **200 W** (so 30 V is available only up to about 6.7 A, and 20 A
only up to 10 V). LAN socket port is **5025**. `*IDN?` looks like
`Keysight Technologies,E36233A,<serial>,<firmware>`.

Output 1 and output 2 are selected with `INST:NSEL 1` and `INST:NSEL 2`, then
the same `VOLT` / `CURR` / `OUTP` / `MEAS` commands as the E36231A. **Fetch**
reads both outputs even when they are off, and shows the latest voltage and
current as digits. From the second reading onward, those samples are plotted
against session time. CSV and JSON exports use that same series, one row per
reading. The channel panel sets voltage and current limit.

**Pairing** (`OUTP:PAIR`) can leave the outputs independent, stack them in
**series** (voltages add, up to 60 V), or tie them in **parallel** (currents
add, up to 40 A). In series or parallel, output 2 follows output 1.

The dual 60 V / 10 A E36234A uses the same commands. Verified against an
E36233A on firmware 1.1.1-1.0.3-1.01.

```bash
ping -c 3 <psu-ip>
nc -z -v <psu-ip> 5025
cargo run --release -- --host <psu-ip> --port 5025 get
```

## Rigol DS1000Z / MSO1000Z

Oscilloscopes (DS1054Z, DS1074Z, DS1104Z and their MSO logic-analyzer variants;
port **5555**). `*IDN?` looks like
`RIGOL TECHNOLOGIES,MSO1104Z,<serial>,<firmware>`; DS1000Z and MSO1000Z share
one SCPI command set (PGA19110-1110).

The app reads and writes channel, timebase, edge-trigger and acquisition state,
and pulls `WORD` waveforms in `NORM`/`RAW` modes. Inputs are fixed at 1 MΩ and
bandwidth is 20 MHz or the model's full bandwidth (50/70/100 MHz by model).

## Rigol DM3058 / DM3058E

A 5½-digit bench **multimeter** (port **5555**). The function is the "wave type"
selector (DCV/ACV/DCI/ACI/2-wire/4-wire/frequency/period/capacitance/continuity/
diode); each **Fetch** takes one `:MEASure:<function>?` reading and shows it as
large digits with its unit.

## Rigol DSA800

A **spectrum analyzer** (DSA815/DSA832/DSA875, port **5555**). The left panel
sets center and span (`:FREQ:CENT` / `:FREQ:SPAN`); **Fetch** reads
`:TRACe:DATA? TRACE1` and plots it in Hz/dBm.

## Hantek HDM3000

A bench **multimeter** (port **5025**, Agilent-style command set). The function
selector writes `FUNC "<function>"` and **Fetch** performs `READ?`. TEMP
readings are shown in °C.

## Hantek DAQ4000A

A **scanning DAQ / multimeter** (port **5025**). The app reads the instrument's
`ROUTe:SCAN` channel list and its `FUNC?` per channel, applies a function to the
whole list, and shows one large reading per scanned channel from a single
comma-separated `READ?` scan. Strain and temperature-probe variants are not
exposed yet — they need bridge/probe configuration to be meaningful.

## Hantek HRDO2000 series

Hantek **digital oscilloscopes** (port **5025**). Channel, timebase,
edge-trigger and acquisition controls follow the DS1000Z-style spelling, with
Hantek's own run/stop (`:RUNing ON|OFF`), single-shot (`:SINGle`), and `*IDN?`
names like `Hantek,HRDO2204,…`.

The waveform transfer has no `:WAVeform:PREamble?`. `:WAVeform:DATA:DISP?`
returns a fixed **128-byte binary header** (per-channel offsets, vertical
scales, sample rate, pre-trigger time) followed by one byte per sample — see
[Protocol & quirks](protocol.md) for the field layout and scaling.

## Siglent SSA / SVA / SHA

**Spectrum analyzers** (SSA3000X, SSA3000X Plus, SSA3000X-R, SSA5000A, SVA1000X,
SHA800A; port **5025**). Center/span control and a `TRACE1` fetch plot in
Hz/dBm, like the DSA800.

This driver is **best-effort**: the manual shipped with the project is an IVI-C
driver guide with no SCPI strings, so the command set comes from the publicly
documented SSA3000X series. Every control query falls back to a default and the
trace parser accepts ASCII or `REAL,32` in either byte order, but check a first
connection against the instrument.

## Discovery and connection

**Scan** browses mDNS LXI (`_lxi._tcp`, `_scpi-raw._tcp`) and probes ARP
neighbors, but only IPv4 link-local addresses (`169.254.1.0`–`169.254.254.255`).
It does not probe the Wi-Fi or any other routed LAN. Those hosts are queried on
ports 4000, 5555 and 5025 with `*IDN?`. It also lists **USB TMC** instruments
(USB class `0xFE` / subclass `0x03`), probes `*IDN?` over USBTMC, and fills Host
with an address like `usb:0699:0408:<serial>#0`. Connect opens that USBTMC
interface (no NI-VISA). A single hit fills Host/Port; several hits appear in the
dropdown. From the CLI: `cargo run --release -- discover`. If a LAN port
refuses TCP, Connect tries the other standard ports (4000/5555/5025).
