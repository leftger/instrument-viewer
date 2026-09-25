//! End-to-end driver tests against a loopback [`MockInstrument`](crate::mock_scpi).
//!
//! These cover the session-bound half of each backend — `read_config`,
//! `fetch_channels`, `acquisition_status`, `apply_*`, `wait_sequence` and
//! `autoset` — which unit tests on the pure parsers cannot reach without
//! hardware.

use std::time::Duration;

use crate::backend::{from_idn, InstrumentKind};
use crate::mock_scpi::{MockInstrument, Rule};
use crate::scpi::ScpiSession;

/// The `WFMOutpre?` reply captured from an MDO3024 at `FV:v1.30`.
const TEK_PREAMBLE: &str = r#"2;16;BIN;RI;MSB;"Ch1, DC coupling, 100.0V/div, 4.000us/div, 10000 points, Sample mode";10000;Y;LINEA;"s";4.0000E-9;-20.0000E-6;0;"V";15.6250E-3;12.8000E+3;0.0E+0;TIM;ANALOG;0.0E+0;0.0E+0;0.0E+0"#;

fn connect(mock: &MockInstrument) -> ScpiSession {
    let mut session =
        ScpiSession::connect(mock.addr(), Duration::from_secs(2)).expect("connect to mock");
    // `resync` runs on connect; keep the preamble behaviour exercised too.
    session.set_preamble(Vec::new()).expect("resync");
    session
}

/// Rules for a fake MDO3024: the YAML profile's command table plus a
/// four-point RIBinary `CURVE?`.
fn tek_rules() -> Vec<Rule> {
    let curve: Vec<u8> = [0x0000u16, 12800, 32767, 0x8000]
        .iter()
        .flat_map(|code| code.to_be_bytes())
        .collect();
    vec![
        Rule::line("*IDN?", "TEKTRONIX,MDO3024,B020857,CF:91.1CT FV:v1.30"),
        Rule::line("SELECT:CH*?", "1"),
        Rule::line("CH*:SCALE?", "1.0"),
        Rule::line("CH*:POSITION?", "2.0"),
        Rule::line("CH*:OFFSET?", "0.5"),
        Rule::line("CH*:COUPLING?", "DC"),
        Rule::line("CH*:TERMINATION?", "1000000"),
        Rule::line("CH*:BANDWIDTH?", "20000000"),
        Rule::line("CH*:PROBE:GAIN?", "10"),
        Rule::line("CH*:PROBE:ID:TYPE?", "\"P6139B\""),
        Rule::line("HORIZONTAL:SCALE?", "4.0e-6"),
        Rule::line("HORIZONTAL:POSITION?", "-20.0e-6"),
        Rule::line("HORIZONTAL:RECORDLENGTH?", "8"),
        Rule::line("TRIGGER:A:MODE?", "NORMAL"),
        Rule::line("TRIGGER:A:EDGE:SOURCE?", "CH1"),
        Rule::line("TRIGGER:A:EDGE:SLOPE?", "RISE"),
        Rule::line("TRIGGER:A:EDGE:COUPLING?", "DC"),
        Rule::line("TRIGGER:A:LEVEL:CH1?", "0.25"),
        Rule::line("ACQUIRE:MODE?", "SAMPLE"),
        Rule::line("ACQUIRE:STOPAFTER?", "RUNSTOP"),
        Rule::line("ACQUIRE:STATE?", "0"),
        Rule::line("WFMOutpre?", TEK_PREAMBLE),
        Rule::block("CURVE?", curve),
    ]
}

/// Tektronix MDO3000 through its YAML profile: the generic engine for the
/// command table, and the RIBinary waveform path for `CURVE?`.
#[test]
fn tek_profile_end_to_end() {
    let mock = MockInstrument::start(tek_rules());
    let backend = from_idn("TEKTRONIX,MDO3024,B020857,CF:91.1CT FV:v1.30");
    let mut session = connect(&mock);

    assert_eq!(backend.name(), "Tektronix");
    assert_eq!(backend.kind(), InstrumentKind::Oscilloscope);
    assert_eq!(backend.capabilities().channel_count, 4);
    assert_eq!(
        backend.preamble(),
        vec!["HEADER OFF".to_string(), "VERBOSE OFF".to_string()]
    );

    let config = backend.read_config(&mut session).expect("read config");
    assert_eq!(config.channels.len(), 4);
    let ch1 = &config.channels[0];
    assert!(ch1.enabled);
    assert_eq!(ch1.scale, 1.0);
    assert_eq!(ch1.position, 2.0);
    assert_eq!(ch1.offset, 0.5);
    assert_eq!(ch1.coupling, "DC");
    assert_eq!(ch1.termination_ohms, 1e6);
    assert_eq!(ch1.bandwidth_hz, 20e6);
    assert_eq!(ch1.probe_gain, 10.0);
    assert_eq!(ch1.probe_type, "P6139B");
    assert_eq!(config.horizontal.scale, 4e-6);
    assert_eq!(config.horizontal.position, -20e-6);
    assert_eq!(config.horizontal.record_length, 8);
    assert_eq!(config.trigger.mode, "NORMAL");
    assert_eq!(config.trigger.source, "CH1");
    assert_eq!(config.trigger.slope, "RISE");
    assert_eq!(config.trigger.level, 0.25);
    assert_eq!(config.acquisition.mode, "SAMPLE");
    assert!(!config.acquisition.running);

    let traces = backend
        .fetch_channels(&mut session, &["CH1".to_string()])
        .expect("fetch");
    let trace = &traces[0];
    assert_eq!(trace.channel, "CH1");
    assert_eq!((trace.x_unit.as_str(), trace.y_unit.as_str()), ("s", "V"));
    assert_eq!(trace.points.len(), 4);
    assert!((trace.points[0][0] - (-20e-6)).abs() < 1e-15);
    assert!((trace.points[0][1] - -200.0).abs() < 1e-9);
    assert!((trace.points[1][1]).abs() < 1e-9);
    assert!((trace.points[2][1] - 311.984375).abs() < 1e-9);
    assert!((trace.points[3][1] - -712.0).abs() < 1e-9);

    let status = backend.acquisition_status(&mut session).expect("status");
    assert!(!status.running);
    assert_eq!(status.display, "STOP");
    backend
        .wait_sequence(&mut session, Duration::from_secs(2))
        .expect("wait sequence");
    backend.autoset(&mut session).expect("autoset");

    let channel = config.channels[0].clone();
    backend
        .apply_channel(&mut session, 1, &channel)
        .expect("apply channel");
    backend
        .apply_horizontal(&mut session, &config.horizontal)
        .expect("apply horizontal");
    backend
        .apply_trigger(&mut session, &config.trigger)
        .expect("apply trigger");
    backend
        .apply_acquisition(&mut session, "SAMPLE", "RUNSTOP", false)
        .expect("apply acquisition");
}

/// Rigol DS1000Z/MSO1000Z: its own command spelling, and a RAW chunked
/// `:WAV:DATA?` transfer.
#[test]
fn ds1000z_end_to_end() {
    let curve: Vec<u8> = [127u16, 128, 255, 0]
        .iter()
        .flat_map(|code| code.to_le_bytes())
        .collect();
    let mock = MockInstrument::start(vec![
        Rule::line("*IDN?", "RIGOL TECHNOLOGIES,MSO1104Z,DS1ZD,00.04.03.SP2"),
        Rule::line(":CHAN*:DISP?", "1"),
        Rule::line(":CHAN*:SCAL?", "0.5"),
        Rule::line(":CHAN*:OFFS?", "0.0"),
        Rule::line(":CHAN*:COUP?", "DC"),
        Rule::line(":CHAN*:BWL?", "OFF"),
        Rule::line(":CHAN*:PROB?", "10"),
        Rule::line(":TIM:SCAL?", "1.0e-5"),
        Rule::line(":ACQ:MDEP?", "12000"),
        Rule::line(":ACQ:TYPE?", "NORM"),
        Rule::line(":TRIG:SWE?", "AUTO"),
        Rule::line(":TRIG:EDGE:SOUR?", "CHAN1"),
        Rule::line(":TRIG:EDGE:SLOP?", "POS"),
        Rule::line(":TRIG:COUP?", "DC"),
        Rule::line(":TRIG:EDGE:LEV?", "0.0"),
        Rule::line(":TRIG:STAT?", "STOP"),
        Rule::line(":WAV:PRE?", "1,2,4,1,4.0e-9,-2.4e-5,0,4.0e-2,0,127"),
        Rule::block(":WAV:DATA?", curve),
    ]);
    let backend = from_idn("RIGOL TECHNOLOGIES,MSO1104Z,DS1ZD,00.04.03.SP2");
    let mut session = connect(&mock);

    assert_eq!(backend.name(), "Rigol MSO1104Z");
    assert_eq!(backend.kind(), InstrumentKind::Oscilloscope);
    assert!(backend.command_table().record_length.is_none());

    let config = backend.read_config(&mut session).expect("read config");
    assert_eq!(config.channels.len(), 4);
    assert_eq!(config.channels[0].scale, 0.5);
    assert_eq!(config.channels[0].probe_gain, 10.0);
    assert_eq!(config.horizontal.scale, 1e-5);
    assert_eq!(config.horizontal.record_length, 12_000);

    let traces = backend
        .fetch_channels(&mut session, &["CH1".to_string(), "CH2".to_string()])
        .expect("fetch");
    assert_eq!(traces.len(), 2);
    assert_eq!(traces[0].points.len(), 4);
    // x = xorigin + xincrement * (i - xreference)
    assert!((traces[0].points[0][0] - (-2.4e-5)).abs() < 1e-18);
    // V = (raw - yorigin - yreference) * yincrement
    assert!((traces[0].points[0][1] - 0.0).abs() < 1e-12);
    assert!((traces[0].points[1][1] - 0.04).abs() < 1e-12);
    assert!((traces[0].points[2][1] - 5.12).abs() < 1e-12);
    assert!((traces[0].points[3][1] - (-5.08)).abs() < 1e-12);

    let status = backend.acquisition_status(&mut session).expect("status");
    assert!(!status.running);
    backend
        .wait_sequence(&mut session, Duration::from_secs(2))
        .expect("wait sequence");
    let channel = config.channels[0].clone();
    backend
        .apply_channel(&mut session, 1, &channel)
        .expect("apply channel");
    backend
        .apply_horizontal(&mut session, &config.horizontal)
        .expect("apply horizontal");
    backend
        .apply_trigger(&mut session, &config.trigger)
        .expect("apply trigger");
    backend.autoset(&mut session).expect("autoset");
}

/// Rigol DSA800 spectrum analyzer: ASCII `TRACE1` de-embedded onto the sweep.
#[test]
fn dsa800_spectrum_end_to_end() {
    let mock = MockInstrument::start(vec![
        Rule::line("*IDN?", "RIGOL TECHNOLOGIES,DSA815,DSA8A,00.01.17"),
        Rule::line(":FREQ:SPAN?", "2.0e6"),
        Rule::line(":FREQ:CENT?", "1.0e9"),
        Rule::line(":FREQ:STAR?", "999000000"),
        Rule::line(":FREQ:STOP?", "1001000000"),
        Rule::block(":TRACE:DATA?", b"-10.0,-20.5,-30.25".to_vec()),
    ]);
    let backend = from_idn("RIGOL TECHNOLOGIES,DSA815,DSA8A,00.01.17");
    let mut session = connect(&mock);

    assert_eq!(backend.kind(), InstrumentKind::Spectrum);
    let config = backend.read_config(&mut session).expect("read config");
    assert_eq!(config.horizontal.position, 1e9);
    assert_eq!(config.horizontal.scale, 2e6);

    let traces = backend
        .fetch_channels(&mut session, &["TRACE1".to_string()])
        .expect("fetch");
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0].x_unit, "Hz");
    assert_eq!(traces[0].y_unit, "dBm");
    assert_eq!(traces[0].points.len(), 3);
    assert!((traces[0].points[0][0] - 999e6).abs() < 1.0);
    assert!((traces[0].points[1][0] - 1000e6).abs() < 1.0);
    assert!((traces[0].points[2][1] - -30.25).abs() < 1e-9);

    let status = backend.acquisition_status(&mut session).expect("status");
    assert!(status.display.contains("1.000000e9"));
}

/// Hantek HDM3000 bench multimeter: `FUNC "..."` + `READ?`.
#[test]
fn hdm3000_multimeter_end_to_end() {
    let mock = MockInstrument::start(vec![
        Rule::line("*IDN?", "Hantek,HDM3000,HDM3A,1.02"),
        Rule::line("FUNC?", "\"VOLT:DC\""),
        Rule::line("READ?", "+1.234500E+00"),
    ]);
    let backend = from_idn("Hantek,HDM3000,HDM3A,1.02");
    let mut session = connect(&mock);

    assert_eq!(backend.kind(), InstrumentKind::Multimeter);
    assert_eq!(backend.name(), "Hantek HDM3000");
    let config = backend.read_config(&mut session).expect("read config");
    assert_eq!(config.channels[0].wave_type, "DCV");
    assert_eq!(config.channels[0].probe_type, "V");

    let traces = backend
        .fetch_channels(&mut session, &["CH1".to_string()])
        .expect("fetch");
    assert_eq!(traces[0].points, vec![[0.0, 1.2345]]);
    assert_eq!(traces[0].y_unit, "V");

    let mut channel = config.channels[0].clone();
    channel.wave_type = "R4W".into();
    backend
        .apply_channel(&mut session, 1, &channel)
        .expect("apply function");
}

/// Hantek DAQ4000A: scan list, per-channel function, comma-separated `READ?`.
#[test]
fn daq4000a_scan_end_to_end() {
    let mock = MockInstrument::start(vec![
        Rule::line("*IDN?", "Hantek,DAQ4000A,DQ4A,1.01"),
        Rule::block("ROUTE:SCAN?", b"(@101,102,103)".to_vec()),
        Rule::line("FUNC?", "\"VOLT:DC\",(@101)"),
        Rule::line("READ?", "+1.0,+2.5,+3.25"),
    ]);
    let backend = from_idn("Hantek,DAQ4000A,DQ4A,1.01");
    let mut session = connect(&mock);

    assert_eq!(backend.kind(), InstrumentKind::Multimeter);
    let config = backend.read_config(&mut session).expect("read config");
    assert_eq!(config.channels.len(), 3);
    assert_eq!(config.channels[0].scale, 1.0);
    assert_eq!(config.channels[1].scale, 2.5);
    assert_eq!(config.channels[2].scale, 3.25);

    let traces = backend
        .fetch_channels(&mut session, &["CH1".to_string()])
        .expect("fetch");
    assert_eq!(traces.len(), 3);
    assert_eq!(traces[0].channel, "CH101");
    assert_eq!(traces[2].points, vec![[0.0, 3.25]]);

    let mut channel = config.channels[0].clone();
    channel.wave_type = "ACV".into();
    backend
        .apply_channel(&mut session, 1, &channel)
        .expect("apply function");
}

/// Hantek HRDO2000: the non-standard 128-byte waveform header.
#[test]
fn hrdo2000_header_block_end_to_end() {
    let payload: Vec<u8> = vec![128, 129, 127, 200];
    let mut block = hrdo_header(payload.len(), "2.400", "1.000e6", 1_000_000);
    block.extend_from_slice(&payload);
    let mock = MockInstrument::start(vec![
        Rule::line("*IDN?", "Hantek,HRDO2204,HRD2A,202606"),
        Rule::line(":CHAN*:DISP?", "1"),
        Rule::line(":CHAN*:SCAL?", "2.4"),
        Rule::line(":CHAN*:OFFS?", "0.0"),
        Rule::line(":CHAN*:COUP?", "DC"),
        Rule::line(":CHAN*:BWL?", "OFF"),
        Rule::line(":CHAN*:PROB?", "10"),
        Rule::line(":TIM:MAIN:SCAL?", "1.0e-4"),
        Rule::line(":ACQ:MDEP?", "10000"),
        Rule::line(":ACQ:TYPE?", "NORM"),
        Rule::line(":TRIG:SWE?", "AUTO"),
        Rule::line(":TRIG:EDGE:SOUR?", "CHAN1"),
        Rule::line(":TRIG:EDGE:SLOP?", "POS"),
        Rule::line(":TRIG:EDGE:LEV?", "0.0"),
        Rule::line(":TRIG:STAT?", "STOP"),
        Rule::raw(":WAVEFORM:DATA:DISP?", block),
    ]);
    let backend = from_idn("Hantek,HRDO2204,HRD2A,202606");
    let mut session = connect(&mock);

    assert_eq!(backend.kind(), InstrumentKind::Oscilloscope);
    let config = backend.read_config(&mut session).expect("read config");
    assert_eq!(config.channels[0].scale, 2.4);

    let traces = backend
        .fetch_channels(&mut session, &["CH1".to_string()])
        .expect("fetch");
    // 2.4 V/div at 10x probe is 1 V per code; the header's -1 V offset is
    // subtracted after scaling, so every reading sits 10 V higher.
    assert_eq!(traces[0].points.len(), 4);
    let codes = [10.0f64, 11.0, 9.0, 82.0];
    for (point, code) in traces[0].points.iter().zip(codes) {
        assert!((point[1] - code).abs() < 1e-9, "{point:?}");
    }
    // t = i / sample_rate - pre_trigger (1 µs pre-trigger at 1 MSa/s)
    assert!((traces[0].points[0][0] - (-1e-6)).abs() < 1e-15);
    assert!((traces[0].points[3][0] - (3e-6 - 1e-6)).abs() < 1e-15);
}

/// Siglent SSA/SVA/SHA: best-effort spectrum driver, ASCII trace.
#[test]
fn siglent_ssa_end_to_end() {
    let mock = MockInstrument::start(vec![
        Rule::line("*IDN?", "Siglent Technologies,SSA3021X,SSA3A,1.3.9.8"),
        Rule::line(":FREQ:CENT?", "1.0e9"),
        Rule::line(":FREQ:SPAN?", "1.0e6"),
        Rule::line(":FREQ:STAR?", "999500000"),
        Rule::line(":FREQ:STOP?", "1000500000"),
        Rule::line(":SWE:POIN?", "1001"),
        Rule::block(":TRACE:DATA?", b"-30.0,-40.0,-50.0".to_vec()),
    ]);
    let backend = from_idn("Siglent Technologies,SSA3021X,SSA3A,1.3.9.8");
    let mut session = connect(&mock);

    assert_eq!(backend.kind(), InstrumentKind::Spectrum);
    assert_eq!(backend.name(), "Siglent SSA3021X");
    let config = backend.read_config(&mut session).expect("read config");
    assert_eq!(config.horizontal.position, 1e9);
    assert_eq!(config.horizontal.scale, 1e6);
    assert_eq!(config.horizontal.record_length, 1001);

    let traces = backend
        .fetch_channels(&mut session, &["TRACE1".to_string()])
        .expect("fetch");
    assert_eq!(traces[0].points.len(), 3);
    assert!((traces[0].points[0][0] - 999.5e6).abs() < 1.0);
    assert!((traces[0].points[0][1] - -30.0).abs() < 1e-9);
    assert_eq!(traces[0].y_unit, "dBm");

    // Center/span are written by `apply_horizontal`.
    backend
        .apply_horizontal(&mut session, &config.horizontal)
        .expect("apply span");
    assert!(backend.autoset(&mut session).is_ok());
}

/// Rigol DM3058 multimeter: function selection plus `:MEASure:<function>?`.
#[test]
fn dm3058_multimeter_end_to_end() {
    let mock = MockInstrument::start(vec![
        Rule::line("*IDN?", "RIGOL TECHNOLOGIES,DM3058,DM3A,00.01.00"),
        Rule::line(":FUNC?", "VOLT"),
        Rule::line("MEAS:VOLT:DC?", "9.876543E-01"),
        Rule::line("MEAS:RES?", "1.2345E+03"),
    ]);
    let backend = from_idn("RIGOL TECHNOLOGIES,DM3058,DM3A,00.01.00");
    let mut session = connect(&mock);

    assert_eq!(backend.kind(), InstrumentKind::Multimeter);
    let config = backend.read_config(&mut session).expect("read config");
    // `:FUNC?` answers "VOLT"; the app calls that DCV and measures accordingly.
    assert_eq!(config.channels[0].wave_type, "DCV");
    assert_eq!(config.channels[0].probe_type, "V");

    let traces = backend
        .fetch_channels(&mut session, &["DMM".to_string()])
        .expect("fetch");
    assert_eq!(traces.len(), 1);
    assert_eq!(traces[0].y_unit, "V");
    assert_eq!(traces[0].points, vec![[0.0, 0.9876543]]);

    // Switching function writes the guide's spelling for 2-wire resistance.
    let mut channel = config.channels[0].clone();
    channel.wave_type = "R2W".into();
    backend
        .apply_channel(&mut session, 1, &channel)
        .expect("apply function");
    // A write has no reply, so round-trip once before reading the log.
    session.query("*OPC?").expect("flush");
    assert!(mock.saw(":FUNC:RES"), "{:?}", mock.commands());
}

/// Siglent SDS1000X-E: horizontal-channel flags, `TRSE`/`TRMD` trigger words
/// and `WF? DAT2` 8-bit two's-complement samples.
#[test]
fn sds_scope_end_to_end() {
    let mock = MockInstrument::start(vec![
        Rule::line("*IDN?", "Siglent Technologies,SDS1104X-E,SN,7.6.1.15"),
        Rule::line("C*:TRA?", "ON"),
        Rule::line("C*:VDIV?", "5.000000E-01"),
        Rule::line("C*:OFST?", "-5.000000E-01"),
        Rule::line("C*:CPL?", "D1M"),
        Rule::line("C*:ATTN?", "1.0E+00"),
        Rule::line("BWL?", "C1,OFF,C2,OFF,C3,OFF,C4,OFF"),
        Rule::line("TRSE?", "EDGE,SR,C1"),
        Rule::line("TRMD?", "AUTO"),
        Rule::line("C1:TRSL?", "POS"),
        Rule::line("C1:TRCP?", "DC"),
        Rule::line("C1:TRLV?", "0.0E+00"),
        Rule::line("TDIV?", "5.0E-09"),
        Rule::line("MSIZ?", "10K"),
        Rule::line("SARA?", "1.0E+09"),
        Rule::line("SAST?", "Stop"),
        Rule::block("C1:WF? DAT2", vec![2, 0xFC, 0x80, 0x7F]),
    ]);
    let backend = from_idn("Siglent Technologies,SDS1104X-E,SN,7.6.1.15");
    let mut session = connect(&mock);

    assert_eq!(backend.kind(), InstrumentKind::Oscilloscope);
    assert_eq!(backend.name(), "Siglent SDS1104X-E");
    assert_eq!(backend.capabilities().channel_count, 4);

    let config = backend.read_config(&mut session).expect("read config");
    assert_eq!(config.channels.len(), 4);
    assert!(config.channels[0].enabled);
    assert_eq!(config.channels[0].scale, 0.5);
    assert_eq!(config.channels[0].offset, -0.5);
    assert_eq!(config.channels[0].coupling, "DC");
    assert_eq!(config.horizontal.scale, 5e-9);
    assert_eq!(config.horizontal.record_length, 10_000);
    assert_eq!(config.trigger.source, "C1");
    assert_eq!(config.trigger.slope, "POS");
    // `TRMD?` answers AUTO, so the sweep is not stopped; `SAST?` below is the
    // separate run/stop axis.
    assert!(config.acquisition.running);

    // Manual worked example: vdiv 0.5 V, offset -0.5 V, codes 2/-4/-128/127 at
    // 1 GSa/s from t0 = -(5 ns x 14 / 2).
    let traces = backend
        .fetch_channels(&mut session, &["C1".to_string()])
        .expect("fetch");
    let points = &traces[0].points;
    assert_eq!(traces[0].x_unit, "s");
    assert_eq!(traces[0].y_unit, "V");
    assert_eq!(points.len(), 4);
    assert!((points[0][0] - (-35e-9)).abs() < 1e-18);
    assert!((points[1][0] - (-34e-9)).abs() < 1e-18);
    assert!((points[0][1] - 0.54).abs() < 1e-9);
    assert!((points[1][1] - 0.42).abs() < 1e-9);
    assert!((points[2][1] - (-2.06)).abs() < 1e-9);
    assert!((points[3][1] - 3.04).abs() < 1e-9);

    let status = backend.acquisition_status(&mut session).expect("status");
    assert_eq!(status.display, "STOP");
    backend
        .wait_sequence(&mut session, Duration::from_secs(2))
        .expect("wait sequence");

    let channel = config.channels[0].clone();
    backend
        .apply_channel(&mut session, 1, &channel)
        .expect("apply channel");
    backend
        .apply_horizontal(&mut session, &config.horizontal)
        .expect("apply horizontal");
    backend
        .apply_trigger(&mut session, &config.trigger)
        .expect("apply trigger");
    backend.autoset(&mut session).expect("autoset");
    session.query("*OPC?").expect("flush");
    assert!(mock.saw("TDIV "), "{:?}", mock.commands());
    assert!(mock.saw("TRMD "), "{:?}", mock.commands());
    assert!(mock.saw("ASET"), "{:?}", mock.commands());
}

/// Keysight E36233A supply: two selectable outputs, a setpoint readback and
/// series/parallel pairing. `MEAS:*?` rules come first because a bare `VOLT?`
/// rule would also match them.
#[test]
fn keysight_supply_end_to_end() {
    let mock = MockInstrument::start(vec![
        Rule::line("*IDN?", "Keysight Technologies,E36233A,SN,1.1.1-1.0.3-1.01"),
        Rule::line("MEAS:VOLT?", "12.004"),
        Rule::line("MEAS:CURR?", "0.512"),
        Rule::line("OUTP:PAIR?", "OFF"),
        Rule::line("OUTP?", "1"),
        Rule::line("VOLT?", "12.0"),
        Rule::line("CURR?", "0.5"),
        Rule::line("SYST:ERR?", "0,\"No error\""),
    ]);
    let backend = from_idn("Keysight Technologies,E36233A,SN,1.1.1-1.0.3-1.01");
    let mut session = connect(&mock);

    assert_eq!(backend.kind(), InstrumentKind::Supply);
    assert_eq!(backend.name(), "Keysight E36233A");
    assert_eq!(backend.capabilities().channel_count, 2);
    assert!(backend.capabilities().output_pairs.len() >= 2);
    assert!(backend.waveform_format().is_none());

    let config = backend.read_config(&mut session).expect("read config");
    assert_eq!(config.channels.len(), 2);
    assert!(config.channels[0].enabled);
    assert_eq!(config.channels[0].scale, 12.0);
    assert_eq!(config.channels[0].offset, 0.5);
    assert_eq!(config.channels[1].scale, 12.0);

    let traces = backend
        .fetch_channels(&mut session, &["CH1".to_string(), "CH2".to_string()])
        .expect("fetch");
    assert_eq!(traces.len(), 4);
    assert_eq!(traces[0].channel, "CH1 Voltage");
    assert_eq!(traces[0].y_unit, "V");
    assert_eq!(traces[0].points, vec![[0.0, 12.004]]);
    assert_eq!(traces[1].channel, "CH1 Current");
    assert_eq!(traces[1].points, vec![[0.0, 0.512]]);
    assert_eq!(traces[3].channel, "CH2 Current");

    let status = backend.acquisition_status(&mut session).expect("status");
    assert!(status.running);
    assert!(status.display.contains("12.004 V"), "{}", status.display);
    assert!(backend.autoset(&mut session).is_err());

    let channel = config.channels[0].clone();
    backend
        .apply_channel(&mut session, 1, &channel)
        .expect("apply channel");
    // Pairing lives in free functions rather than the trait, since only supplies
    // with a pairing mode support it.
    assert_eq!(
        crate::keysight::read_output_pair(&mut session, backend.as_ref()).expect("read pair"),
        "OFF"
    );
    crate::keysight::apply_output_pair(&mut session, backend.as_ref(), "SERIES")
        .expect("pair outputs");
    session.query("*OPC?").expect("flush");
    assert!(mock.saw("INST:NSEL 1"), "{:?}", mock.commands());
    assert!(mock.saw("OUTP:PAIR SERIES"), "{:?}", mock.commands());
}

/// Rigol DHO900 through the generic `rigol` driver: Rigol-native channel
/// spelling mixed with the Tektronix timebase/trigger vocabulary.
#[test]
fn rigol_dho900_end_to_end() {
    // 1000 WORD samples: mostly mid-scale, with a few known codes.
    let mut samples = vec![128u16; 1000];
    samples[0] = 0;
    samples[1] = 255;
    samples[3] = 64;
    let curve: Vec<u8> = samples.iter().flat_map(|code| code.to_le_bytes()).collect();
    let mock = MockInstrument::start(vec![
        Rule::line("*IDN?", "RIGOL TECHNOLOGIES,DHO924S,SN,00.01.05"),
        Rule::line(":CHAN*:DISP?", "1"),
        Rule::line(":CHAN*:IMP?", "OMEG"),
        Rule::line(":CHAN*:BWL?", "20M"),
        Rule::line("CH*:SCALE?", "0.5"),
        Rule::line("CH*:POSITION?", "0.0"),
        Rule::line("CH*:OFFSET?", "0.0"),
        Rule::line("CH*:COUPLING?", "DC"),
        Rule::line("CH*:PROBE:GAIN?", "10"),
        Rule::line("TRIGGER:A:LEVEL?", "0.0"),
        Rule::line("TRIGGER:A:MODE?", "NORMAL"),
        Rule::line("TRIGGER:A:EDGE:SOURCE?", "CH1"),
        Rule::line("TRIGGER:A:EDGE:SLOPE?", "RISE"),
        Rule::line("TRIGGER:A:EDGE:COUPLING?", "DC"),
        Rule::line("ACQUIRE:MODE?", "SAMPLE"),
        Rule::line("ACQUIRE:STOPAFTER?", "RUNSTOP"),
        Rule::line("ACQUIRE:STATE?", "0"),
        Rule::line("HORIZONTAL:SCALE?", "1.0E-06"),
        Rule::line("HORIZONTAL:POSITION?", "0.0"),
        Rule::line("HORIZONTAL:RECORDLENGTH?", "1000"),
        Rule::line(":ACQ:MDEP?", "1000"),
        Rule::line(":TRIG:STAT?", "STOP"),
        Rule::line(":WAV:PRE?", "1,2,1000,1,1.0e-6,0,0,0.03125,0,128"),
        Rule::block(":WAV:DATA?", curve),
    ]);
    let backend = from_idn("RIGOL TECHNOLOGIES,DHO924S,SN,00.01.05");
    let mut session = connect(&mock);

    assert_eq!(backend.kind(), InstrumentKind::Oscilloscope);
    assert_eq!(backend.name(), "Rigol DHO924S");
    let config = backend.read_config(&mut session).expect("read config");
    assert_eq!(config.channels.len(), 4);
    // The DHO900 input is fixed 1 MOhm and its bandwidth control is the 20 MHz
    // limit, both reported from the Rigol-native replies above.
    assert_eq!(config.channels[0].termination_ohms, 1e6);
    assert_eq!(config.channels[0].bandwidth_hz, 20e6);
    assert_eq!(config.channels[0].probe_gain, 10.0);
    assert_eq!(config.horizontal.record_length, 1000);

    let traces = backend
        .fetch_channels(&mut session, &["CH1".to_string()])
        .expect("fetch");
    assert_eq!(traces[0].points.len(), 1000);
    // V = (raw - yorigin - yreference) * yincrement
    assert!((traces[0].points[0][1] - (-4.0)).abs() < 1e-12);
    assert!((traces[0].points[1][1] - 3.96875).abs() < 1e-12);
    assert_eq!(traces[0].points[2][1], 0.0);
    assert!((traces[0].points[3][1] - (-2.0)).abs() < 1e-12);

    let status = backend.acquisition_status(&mut session).expect("status");
    assert!(!status.running);
    backend.autoset(&mut session).expect("autoset");
    backend
        .wait_sequence(&mut session, Duration::from_secs(2))
        .expect("wait sequence");
}

/// Tektronix AFG3000 generator: programmed wave readback and preview.
#[test]
fn afg_generator_end_to_end() {
    let mock = MockInstrument::start(vec![
        Rule::line("*IDN?", "TEKTRONIX,AFG3051C,SN,1.0"),
        Rule::line("OUTP1:STAT?", "1"),
        Rule::line("OUTP1:IMP?", "50"),
        Rule::line("SOUR1:FREQ?", "1.0E+06"),
        Rule::line("SOUR1:FUNC:SHAP?", "SINE"),
        Rule::line("SOUR1:VOLT:AMPL?", "2.0"),
        Rule::line("SOUR1:VOLT:OFFS?", "0.5"),
    ]);
    let backend = from_idn("TEKTRONIX,AFG3051C,SN,1.0");
    let mut session = connect(&mock);

    assert_eq!(backend.kind(), InstrumentKind::Generator);
    assert_eq!(backend.name(), "Tektronix AFG3051C");
    assert!(backend.waveform_format().is_none());

    let config = backend.read_config(&mut session).expect("read config");
    assert!(!config.channels.is_empty());
    assert_eq!(config.channels[0].wave_type, "SINE");
    assert_eq!(config.channels[0].scale, 2.0);
    assert_eq!(config.channels[0].offset, 0.5);
    assert_eq!(config.channels[0].frequency_hz, 1e6);
    assert!(config.channels[0].enabled);

    // Fetch previews the programmed wave rather than digitising a BNC.
    let traces = backend
        .fetch_channels(&mut session, &["CH1".to_string()])
        .expect("fetch preview");
    assert_eq!(traces.len(), 1);
    assert!(traces[0].points.len() > 100);
    let peak = traces[0]
        .points
        .iter()
        .map(|point| point[1])
        .fold(f64::MIN, f64::max);
    assert!((0.5..=2.5).contains(&peak), "peak {peak}");

    let channel = config.channels[0].clone();
    backend
        .apply_channel(&mut session, 1, &channel)
        .expect("apply channel");
    session.query("*OPC?").expect("flush");
    assert!(mock.saw("SOUR1:FREQ"), "{:?}", mock.commands());
}

/// Siglent SDG1000X generator: `C<n>:*` output control and preview.
#[test]
fn siglent_sdg_generator_end_to_end() {
    let mock = MockInstrument::start(vec![
        Rule::line("*IDN?", "Siglent Technologies,SDG1032X,SN,1.0"),
        Rule::line("C1:OUTP?", "ON,LOAD,HZ,PLRT,NOR"),
        Rule::line("C2:OUTP?", "OFF,LOAD,50,PLRT,NOR"),
        Rule::line(
            "C1:BSWV?",
            "WVTP,SINE,FRQ,1000HZ,AMP,2V,OFST,0V,DUTY,50,PHSE,0",
        ),
        Rule::line("C2:BSWV?", "WVTP,SQUARE,FRQ,2000HZ,AMP,1V,OFST,-1V"),
    ]);
    let backend = from_idn("Siglent Technologies,SDG1032X,SN,1.0");
    let mut session = connect(&mock);

    assert_eq!(backend.kind(), InstrumentKind::Generator);
    assert_eq!(backend.name(), "Siglent SDG1032X");
    let config = backend.read_config(&mut session).expect("read config");
    assert_eq!(config.channels.len(), 2);

    let c1 = &config.channels[0];
    assert!(c1.enabled);
    assert_eq!(c1.wave_type, "SINE");
    assert_eq!(c1.frequency_hz, 1000.0);
    assert_eq!(c1.scale, 2.0);
    assert_eq!(c1.offset, 0.0);
    // `LOAD,HZ` reports a high-impedance output.
    assert_eq!(c1.termination_ohms, 1e6);

    let c2 = &config.channels[1];
    assert!(!c2.enabled);
    assert_eq!(c2.wave_type, "SQUARE");
    assert_eq!(c2.frequency_hz, 2000.0);
    assert_eq!(c2.offset, -1.0);
    assert_eq!(c2.termination_ohms, 50.0);

    // Fetch previews the programmed wave: a 2 Vpp sine peaks at 1 V.
    let traces = backend
        .fetch_channels(&mut session, &["CH1".to_string()])
        .expect("fetch preview");
    assert_eq!(traces[0].y_unit, "V");
    assert!(traces[0].points.len() > 100);
    let peak = traces[0]
        .points
        .iter()
        .map(|point| point[1])
        .fold(f64::MIN, f64::max);
    assert!((0.9..=1.1).contains(&peak), "peak {peak}");

    let channel = config.channels[0].clone();
    backend
        .apply_channel(&mut session, 1, &channel)
        .expect("apply channel");
    session.query("*OPC?").expect("flush");
    assert!(mock.saw("C1:BSWV"), "{:?}", mock.commands());
    assert!(mock.saw("C1:OUTP"), "{:?}", mock.commands());
}

/// Build the 128-byte header the HRDO2000 puts in front of its samples.
fn hrdo_header(
    payload_len: usize,
    scale_v: &str,
    sample_rate: &str,
    pretrigger_ps: i64,
) -> Vec<u8> {
    let mut block = vec![0u8; 128];
    block[0] = b'#';
    block[1] = b'9';
    let len = format!("{payload_len:09}");
    block[11..20].copy_from_slice(len.as_bytes());
    block[29] = b'0';
    block[30] = b'1';
    block[31..35].copy_from_slice(&(-1_000_000i32).to_le_bytes());
    let mut field = [b' '; 7];
    field[..scale_v.len()].copy_from_slice(scale_v.as_bytes());
    block[47..54].copy_from_slice(&field);
    field = [b' '; 7];
    field[..3].copy_from_slice(b"1.0");
    block[54..61].copy_from_slice(&field);
    let mut rate = [b' '; 9];
    rate[..sample_rate.len()].copy_from_slice(sample_rate.as_bytes());
    block[79..88].copy_from_slice(&rate);
    block[94..102].copy_from_slice(&0i64.to_le_bytes());
    block[103..111].copy_from_slice(&pretrigger_ps.to_le_bytes());
    block
}

/// Drive the command-line surface against the mock scope. These run the real
/// `cli::run` dispatch, including its SCPI session handling and file writes.
#[cfg(test)]
mod cli {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;

    fn parse(args: &[&str]) -> Cli {
        let mut argv = vec!["instrument-viewer"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).expect("parse cli")
    }

    /// Run one subcommand against `mock`.
    fn run(mock: &MockInstrument, args: &[&str]) -> Result<(), Box<dyn std::error::Error>> {
        let port = mock
            .addr()
            .rsplit(':')
            .next()
            .expect("mock port")
            .to_string();
        let mut full: Vec<&str> = vec!["--host", "127.0.0.1", "--port", &port];
        full.extend_from_slice(args);
        let mut cli = parse(&full);
        let command = cli.command.take().expect("subcommand");
        crate::cli::run(&cli, &command)
    }

    #[test]
    fn get_reads_every_section() {
        let mock = MockInstrument::start(tek_rules());
        run(&mock, &["get"]).expect("get");
    }

    #[test]
    fn scpi_query_and_write_reach_the_instrument() {
        let mock = MockInstrument::start(tek_rules());
        run(&mock, &["scpi", "query", "CH1:SCALE?"]).expect("query");
        run(&mock, &["scpi", "write", "CH1:SCALE 0.5"]).expect("write");
    }

    #[test]
    fn settings_subcommands_apply() {
        let mock = MockInstrument::start(tek_rules());
        run(
            &mock,
            &[
                "channel",
                "1",
                "--enabled",
                "true",
                "--probe",
                "10",
                "--scale",
                "500mV",
                "--coupling",
                "dc",
                "--termination",
                "one-meg",
                "--bandwidth",
                "20MHz",
            ],
        )
        .expect("channel");
        run(
            &mock,
            &[
                "horizontal",
                "--scale",
                "2us",
                "--position",
                "50",
                "--record-length",
                "10k",
            ],
        )
        .expect("horizontal");
        run(
            &mock,
            &[
                "trigger",
                "--mode",
                "normal",
                "--source",
                "ch1",
                "--slope",
                "rise",
                "--coupling",
                "dc",
                "--level",
                "250mV",
            ],
        )
        .expect("trigger");
        run(
            &mock,
            &[
                "acquisition",
                "--mode",
                "average",
                "--stop-after",
                "sequence",
                "--running",
                "true",
            ],
        )
        .expect("acquisition");
        run(&mock, &["autoset"]).expect("autoset");
    }

    #[test]
    fn export_writes_csv_json_and_wide_files() {
        let mock = MockInstrument::start(tek_rules());
        let dir = std::env::temp_dir().join(format!("iv-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let csv = dir.join("capture.csv");
        run(&mock, &["export", "--out", csv.to_str().unwrap()]).expect("csv");
        let text = std::fs::read_to_string(&csv).unwrap();
        assert!(text.contains("CH1"), "{text}");

        let json = dir.join("capture.json");
        run(&mock, &["export", "--out", json.to_str().unwrap()]).expect("json");
        let text = std::fs::read_to_string(&json).unwrap();
        assert!(text.contains("\"channel\""), "{text}");

        let wide = dir.join("wide.csv");
        run(
            &mock,
            &[
                "export",
                "--wide",
                "--channels",
                "CH1",
                "--out",
                wide.to_str().unwrap(),
            ],
        )
        .expect("wide csv");
        let text = std::fs::read_to_string(&wide).unwrap();
        // Comment lines carry the settings snapshot; the header follows.
        assert!(text.contains("\nt,CH1\n"), "{text}");

        // An unknown suffix is rejected by `resolve_format`.
        let bad = dir.join("capture.txt");
        let err = run(&mock, &["export", "--out", bad.to_str().unwrap()]).unwrap_err();
        assert!(err.to_string().contains("unknown export format"), "{err}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn export_sequence_arms_and_waits() {
        let mock = MockInstrument::start(tek_rules());
        let dir = std::env::temp_dir().join(format!("iv-cli-seq-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("shot.json");
        run(
            &mock,
            &["export", "--sequence", "--out", out.to_str().unwrap()],
        )
        .expect("sequence");
        assert!(out.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}

/// Tektronix fallback driver (anything Tek that is not an MDO3000 profile).
#[test]
fn tek_fallback_driver_end_to_end() {
    let mock = MockInstrument::start(tek_rules());
    let backend = from_idn("TEKTRONIX,TDS2024C,SN,CF:91.1CT");
    let mut session = connect(&mock);

    assert_eq!(backend.name(), "Tektronix");
    assert_eq!(backend.kind(), InstrumentKind::Oscilloscope);
    // The fallback driver serves the same command table as the YAML profile.
    assert!(backend.command_table().channel_enabled.is_some());

    let config = backend.read_config(&mut session).expect("read config");
    assert_eq!(config.channels.len(), 4);
    let traces = backend
        .fetch_channels(&mut session, &["CH1".to_string()])
        .expect("fetch");
    assert_eq!(traces[0].points.len(), 4);
    let status = backend.acquisition_status(&mut session).expect("status");
    assert_eq!(status.display, "STOP");
    backend.autoset(&mut session).expect("autoset");
}

/// Drive the GUI's worker thread exactly as the window does, so the connect /
/// identify / read / fetch / poll loop is covered without a display.
#[test]
fn worker_drives_a_mock_scope() {
    use crate::worker::{Cmd, Msg, Worker};

    let mock = MockInstrument::start(tek_rules());
    let worker = Worker::spawn(|| {});

    let wait = |predicate: fn(&Msg) -> bool, what: &str| -> Msg {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut seen = Vec::new();
        while std::time::Instant::now() < deadline {
            if let Some(msg) = worker.try_recv() {
                if predicate(&msg) {
                    return msg;
                }
                seen.push(summarize(&msg));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("timed out waiting for {what}; saw {seen:?}");
    };

    worker.send(Cmd::Connect {
        addr: mock.addr().to_string(),
    });
    match wait(
        |msg| matches!(msg, Msg::Connected { .. } | Msg::Error(_)),
        "connect",
    ) {
        Msg::Connected {
            idn, capabilities, ..
        } => {
            assert!(idn.contains("MDO3024"), "{idn}");
            assert_eq!(capabilities.channel_count, 4);
        }
        Msg::Error(err) => panic!("connect failed: {err}"),
        _ => unreachable!(),
    }

    worker.send(Cmd::ReadConfig);
    match wait(
        |msg| matches!(msg, Msg::Config { .. } | Msg::Error(_)),
        "config",
    ) {
        Msg::Config { config, .. } => assert_eq!(config.channels.len(), 4),
        Msg::Error(err) => panic!("read config failed: {err}"),
        _ => unreachable!(),
    }

    worker.send(Cmd::RawQuery("CH1:SCALE?".into()));
    match wait(
        |msg| matches!(msg, Msg::RawResponse(_) | Msg::Error(_)),
        "raw query",
    ) {
        Msg::RawResponse(reply) => assert_eq!(reply, "1.0"),
        Msg::Error(err) => panic!("raw query failed: {err}"),
        _ => unreachable!(),
    }

    worker.send(Cmd::Fetch {
        channels: vec!["CH1".to_string()],
    });
    match wait(
        |msg| matches!(msg, Msg::Traces { .. } | Msg::Error(_)),
        "fetch",
    ) {
        Msg::Traces { traces, .. } => assert_eq!(traces[0].points.len(), 4),
        Msg::Error(err) => panic!("fetch failed: {err}"),
        _ => unreachable!(),
    }

    worker.send(Cmd::PollStatus);
    match wait(
        |msg| matches!(msg, Msg::AcquisitionStatus(_) | Msg::Error(_)),
        "status",
    ) {
        Msg::AcquisitionStatus(Some(status)) => assert_eq!(status.display, "STOP"),
        other => panic!("unexpected status: {}", summarize(&other)),
    }

    worker.send(Cmd::Autoset);
    let _ = wait(
        |msg| matches!(msg, Msg::Applied(_) | Msg::Error(_)),
        "autoset",
    );

    worker.send(Cmd::Disconnect);
    wait(|msg| matches!(msg, Msg::Disconnected), "disconnect");
}

/// One-line description of a worker message, for test failures.
fn summarize(msg: &crate::worker::Msg) -> String {
    use crate::worker::Msg;
    match msg {
        Msg::Status(text) => format!("Status({text})"),
        Msg::Connected { idn, .. } => format!("Connected({idn})"),
        Msg::Disconnected => "Disconnected".into(),
        Msg::Traces { traces, .. } => format!("Traces({})", traces.len()),
        Msg::Config { .. } => "Config".into(),
        Msg::Applied(text) => format!("Applied({text})"),
        Msg::AcquisitionStatus(_) => "AcquisitionStatus".into(),
        Msg::RawResponse(text) => format!("RawResponse({text})"),
        Msg::Error(text) => format!("Error({text})"),
        Msg::ScanDone { found, .. } => format!("ScanDone({})", found.len()),
    }
}
