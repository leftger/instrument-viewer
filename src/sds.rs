use std::thread;
use std::time::{Duration, Instant};

use crate::backend::{
    channel_number, AcquisitionStatus, Backend, InstrumentCapabilities, InstrumentKind, ValueChoice,
};
use crate::config::{query_bool, ChannelConfig, HorizontalConfig, InstrumentConfig, TriggerConfig};
use crate::profile::WaveformFormat;
use crate::scpi::{parse_f64, ScpiError, ScpiSession};
use crate::waveform::{ChannelTrace, WaveformError};

/// Siglent SDS1000X-E series ("legacy"/LeCroy-derived dialect), covering the
/// SDS1104X-E. Command surface and the WF? DAT2 scaling formula are from the
/// Siglent Digital Oscilloscope Series Programming Guide (PG01-E02D): socket
/// port 5025, `C<n>:TRA`/`VDIV`/`OFST`/`CPL`/`ATTN`, global `BWL`, `TDIV`,
/// `MSIZ`, `SARA?`, `SAST?`, `TRMD`, `TRSE`, and per-source `TRLV`/`TRSL`/`TRCP`.
///
/// `CHDR OFF` (sent from `preamble()`) drops both the header echo and the
/// unit suffix from numeric replies, so `C1:VDIV?` answers a bare `5.00E-01`
/// rather than `C1:VDIV 5.00E-01V`.
pub struct Sds {
    name: String,
    channel_count: usize,
    bandwidth_hz: f64,
}

impl Sds {
    pub fn from_idn(idn: &str) -> Self {
        let model = idn
            .split(',')
            .nth(1)
            .unwrap_or("")
            .trim()
            .to_ascii_uppercase();
        let normalized = model.replace('-', "");
        Self {
            name: if model.is_empty() {
                "Siglent SDS".into()
            } else {
                format!("Siglent {model}")
            },
            channel_count: if normalized.contains("1102") || normalized.contains("1202") {
                2
            } else {
                4
            },
            bandwidth_hz: if normalized.contains("SDS12") {
                200e6
            } else {
                100e6
            },
        }
    }
}

/// Screen width in horizontal divisions, used by the WF? DAT2 time-axis
/// formula (`Programming Guide` p.265: `time = -(timebase*grid/2) + i*dt`).
const GRID_DIVISIONS: f64 = 14.0;
/// 8-bit ADC codes per vertical division (`Programming Guide` p.264).
const CODE_PER_DIV: f64 = 25.0;

impl Backend for Sds {
    fn name(&self) -> &str {
        &self.name
    }

    fn waveform_format(&self) -> Option<WaveformFormat> {
        Some(WaveformFormat::SDS)
    }

    fn kind(&self) -> InstrumentKind {
        InstrumentKind::Oscilloscope
    }

    fn preamble(&self) -> Vec<String> {
        vec!["CHDR OFF".into()]
    }

    fn capabilities(&self) -> InstrumentCapabilities {
        InstrumentCapabilities {
            channel_couplings: strings(&["DC", "AC"]),
            terminations: vec![
                ValueChoice::new("1 MΩ", 1e6),
                ValueChoice::new("50 Ω", 50.0),
            ],
            termination_writable: true,
            bandwidths: vec![
                ValueChoice::new(
                    format!("Full ({:.0} MHz)", self.bandwidth_hz / 1e6),
                    self.bandwidth_hz,
                ),
                ValueChoice::new("20 MHz limit", 20e6),
            ],
            record_lengths: vec![7_000, 70_000, 700_000, 7_000_000, 14_000_000],
            trigger_modes: strings(&["AUTO", "NORM"]),
            trigger_slopes: strings(&["POS", "NEG", "WINDOW"]),
            trigger_couplings: strings(&["DC", "AC", "HFREJ", "LFREJ"]),
            acquisition_modes: strings(&["ACQUIRE"]),
            stop_after: strings(&["RUNSTOP"]),
            channel_hint: None,
            horizontal_hint: Some(
                "Horizontal position is fixed; use the trigger delay from the front panel.".into(),
            ),
            acquisition_hint: Some(
                "Running toggles TRMD AUTO/STOP; use the trigger Mode for AUTO/NORM sweep.".into(),
            ),
            kind: InstrumentKind::Oscilloscope,
            channel_count: self.channel_count,
            wave_types: Vec::new(),
            output_pairs: Vec::new(),
        }
    }

    fn channel_enabled(&self, s: &mut ScpiSession, n: usize) -> Result<bool, ScpiError> {
        query_bool(s, &format!("C{n}:TRA?"))
    }

    fn set_channel_enabled(
        &self,
        s: &mut ScpiSession,
        n: usize,
        on: bool,
    ) -> Result<(), ScpiError> {
        s.write(&format!("C{n}:TRA {}", if on { "ON" } else { "OFF" }))
    }

    fn termination_ohms(&self, s: &mut ScpiSession, n: usize) -> Result<f64, ScpiError> {
        Ok(coupling_from_wire(&s.query(&format!("C{n}:CPL?"))?).1)
    }

    fn set_termination_ohms(
        &self,
        s: &mut ScpiSession,
        n: usize,
        ohms: f64,
    ) -> Result<(), ScpiError> {
        let coupling = coupling_from_wire(&s.query(&format!("C{n}:CPL?"))?).0;
        s.write(&format!("C{n}:CPL {}", coupling_to_wire(&coupling, ohms)))
    }

    fn bandwidth_hz(&self, s: &mut ScpiSession, n: usize) -> Result<f64, ScpiError> {
        Ok(if bwl_enabled(s, n)? {
            20e6
        } else {
            self.bandwidth_hz
        })
    }

    fn set_bandwidth_hz(&self, s: &mut ScpiSession, n: usize, hz: f64) -> Result<(), ScpiError> {
        s.write(&format!(
            "BWL C{n},{}",
            if hz <= 20e6 { "ON" } else { "OFF" }
        ))
    }

    fn probe_type(&self, _s: &mut ScpiSession, _n: usize) -> Result<String, ScpiError> {
        Ok("probe".into())
    }

    fn trigger_level(&self, s: &mut ScpiSession, source: &str) -> Result<f64, ScpiError> {
        parse_f64(&s.query(&format!("{source}:TRLV?"))?)
    }

    fn set_trigger_level(
        &self,
        s: &mut ScpiSession,
        source: &str,
        volts: f64,
    ) -> Result<(), ScpiError> {
        s.write(&format!("{source}:TRLV {volts}"))
    }

    fn apply_acquisition(
        &self,
        s: &mut ScpiSession,
        _mode: &str,
        _stop_after: &str,
        running: bool,
    ) -> Result<(), ScpiError> {
        s.write(if running { "TRMD AUTO" } else { "STOP" })
    }

    fn read_config(&self, s: &mut ScpiSession) -> Result<InstrumentConfig, ScpiError> {
        read_config(s, self)
    }

    fn apply_channel(
        &self,
        s: &mut ScpiSession,
        n: usize,
        ch: &ChannelConfig,
    ) -> Result<(), ScpiError> {
        apply_channel(s, self, n, ch)
    }

    fn apply_horizontal(&self, s: &mut ScpiSession, h: &HorizontalConfig) -> Result<(), ScpiError> {
        s.write(&format!("TDIV {}", h.scale))?;
        s.write(&format!("MSIZ {}", h.record_length))
    }

    fn apply_trigger(&self, s: &mut ScpiSession, t: &TriggerConfig) -> Result<(), ScpiError> {
        s.write(&format!("TRSE EDGE,SR,{}", t.source))?;
        s.write(&format!("{}:TRSL {}", t.source, t.slope))?;
        s.write(&format!("{}:TRCP {}", t.source, t.coupling))?;
        s.write(&format!("{}:TRLV {}", t.source, t.level))?;
        s.write(&format!("TRMD {}", t.mode))
    }

    fn fetch_channel(&self, s: &mut ScpiSession, ch: &str) -> Result<ChannelTrace, WaveformError> {
        let n = channel_number(ch)
            .ok_or_else(|| WaveformError::Parse(format!("SDS channel {ch} is not C1..C4")))?;
        if n > self.channel_count {
            return Err(WaveformError::Parse(format!(
                "{} has {} channel(s)",
                self.name, self.channel_count
            )));
        }
        let vdiv = parse_f64(&s.query(&format!("C{n}:VDIV?"))?)?;
        let offset = parse_f64(&s.query(&format!("C{n}:OFST?"))?)?;
        let tdiv = parse_f64(&s.query("TDIV?")?)?;
        let sample_rate = parse_f64(&s.query("SARA?")?)?;

        s.write("WFSU SP,0,NP,0,FP,0")?;
        let raw = s.query_binary_block(&format!("C{n}:WF? DAT2"))?;

        let t0 = -(tdiv * GRID_DIVISIONS / 2.0);
        let dt = if sample_rate > 0.0 {
            1.0 / sample_rate
        } else {
            0.0
        };
        let points = crate::waveform::decode_samples(
            &WaveformFormat::SDS,
            &raw,
            |i| t0 + i as f64 * dt,
            |code| code * (vdiv / CODE_PER_DIV) - offset,
        )?;

        Ok(ChannelTrace {
            channel: ch.to_string(),
            x_unit: "s".into(),
            y_unit: "V".into(),
            points,
        })
    }

    fn wait_sequence(&self, s: &mut ScpiSession, timeout: Duration) -> Result<(), ScpiError> {
        s.write("TRMD SINGLE")?;
        let start = Instant::now();
        loop {
            thread::sleep(Duration::from_millis(200));
            if read_sast(s)? == "STOP" {
                return Ok(());
            }
            if start.elapsed() > timeout {
                return Err(ScpiError::Timeout);
            }
        }
    }

    fn acquisition_status(&self, s: &mut ScpiSession) -> Result<AcquisitionStatus, ScpiError> {
        let status = read_sast(s)?;
        Ok(AcquisitionStatus {
            running: status != "STOP",
            display: status,
        })
    }

    fn autoset(&self, s: &mut ScpiSession) -> Result<(), ScpiError> {
        s.write("ASET")
    }
}

/// `SAST?` answers e.g. `Trig'd`, `Auto`, `Ready`, `Stop`, `Roll`, `Arm`.
fn read_sast(s: &mut ScpiSession) -> Result<String, ScpiError> {
    let resp = s.query("SAST?")?;
    Ok(resp
        .trim()
        .trim_start_matches("SAST")
        .trim()
        .replace('\'', "")
        .to_ascii_uppercase())
}

/// `BWL?` is global: `C1,OFF,C2,ON,C3,OFF,C4,OFF`.
fn bwl_enabled(s: &mut ScpiSession, n: usize) -> Result<bool, ScpiError> {
    let resp = s.query("BWL?")?;
    let body = resp.trim().trim_start_matches("BWL").trim();
    let tokens: Vec<&str> = body.split(',').map(str::trim).collect();
    let key = format!("C{n}");
    Ok(tokens
        .iter()
        .position(|t| t.eq_ignore_ascii_case(&key))
        .and_then(|i| tokens.get(i + 1))
        .is_some_and(|mode| mode.eq_ignore_ascii_case("ON")))
}

/// `C<n>:CPL?` wire tokens: `D1M`/`D50`/`A1M`/`A50`/`GND`. Returns
/// (coupling, termination_ohms).
fn coupling_from_wire(resp: &str) -> (String, f64) {
    let token = resp
        .trim()
        .rsplit(' ')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_uppercase();
    match token.as_str() {
        "D50" => ("DC".into(), 50.0),
        "A1M" => ("AC".into(), 1e6),
        "A50" => ("AC".into(), 50.0),
        "GND" => ("DC".into(), 1e6),
        _ => ("DC".into(), 1e6), // D1M and anything unrecognized
    }
}

fn coupling_to_wire(coupling: &str, ohms: f64) -> &'static str {
    let ac = coupling.eq_ignore_ascii_case("AC");
    let fifty = ohms < 1000.0;
    match (ac, fifty) {
        (false, false) => "D1M",
        (false, true) => "D50",
        (true, false) => "A1M",
        (true, true) => "A50",
    }
}

pub fn read_config(
    session: &mut ScpiSession,
    backend: &dyn Backend,
) -> Result<InstrumentConfig, ScpiError> {
    let nch = backend.capabilities().channel_count;
    let mut channels = vec![dummy_channel(); nch];
    for n in 1..=nch {
        channels[n - 1] = read_channel(session, backend, n)?;
    }

    let source = trse_source(&session.query("TRSE?")?);
    let mode = trmd_mode(&session.query("TRMD?")?);
    let slope = clean_token(&session.query(&format!("{source}:TRSL?"))?);
    let coupling = clean_token(&session.query(&format!("{source}:TRCP?"))?);
    let level = backend.trigger_level(session, &source)?;
    let running = mode != "STOP";
    let trigger = TriggerConfig {
        // Keep the panel on a real AUTO/NORM sweep mode even when the
        // instrument is currently stopped or single-shot; "Running" (below)
        // is the separate control for that axis.
        mode: if mode == "AUTO" || mode == "NORM" {
            mode
        } else {
            "AUTO".into()
        },
        source,
        slope,
        coupling,
        level,
    };

    let horizontal = HorizontalConfig {
        scale: parse_f64(&session.query("TDIV?")?)?,
        position: 50.0,
        record_length: parse_msiz(&session.query("MSIZ?")?)?,
    };

    Ok(InstrumentConfig {
        channels,
        horizontal,
        trigger,
        acquisition: crate::config::AcquisitionConfig {
            mode: "ACQUIRE".into(),
            stop_after: "RUNSTOP".into(),
            running,
        },
        output_pair: String::new(),
    })
}

fn read_channel(
    session: &mut ScpiSession,
    backend: &dyn Backend,
    n: usize,
) -> Result<ChannelConfig, ScpiError> {
    let enabled = backend.channel_enabled(session, n)?;
    let scale = parse_f64(&session.query(&format!("C{n}:VDIV?"))?)?;
    let offset = parse_f64(&session.query(&format!("C{n}:OFST?"))?)?;
    let (coupling, termination_ohms) = coupling_from_wire(&session.query(&format!("C{n}:CPL?"))?);
    let bandwidth_hz = backend.bandwidth_hz(session, n)?;
    let attn = parse_f64(&session.query(&format!("C{n}:ATTN?"))?)?.max(1e-9);
    Ok(ChannelConfig {
        enabled,
        scale,
        position: 0.0,
        offset,
        coupling,
        termination_ohms,
        bandwidth_hz,
        probe_gain: 1.0 / attn,
        probe_type: "probe".into(),
        wave_type: String::new(),
        frequency_hz: 0.0,
    })
}

pub fn apply_channel(
    session: &mut ScpiSession,
    backend: &dyn Backend,
    n: usize,
    ch: &ChannelConfig,
) -> Result<(), ScpiError> {
    let nch = backend.capabilities().channel_count;
    if n > nch {
        return Err(ScpiError::Unsupported(format!(
            "{} has {nch} channel(s)",
            backend.name()
        )));
    }
    let attn = 1.0 / ch.probe_gain.max(1e-9);
    session.write(&format!("C{n}:ATTN {attn}"))?;
    session.write(&format!(
        "C{n}:CPL {}",
        coupling_to_wire(&ch.coupling, ch.termination_ohms)
    ))?;
    session.write(&format!("C{n}:VDIV {}", ch.scale))?;
    session.write(&format!("C{n}:OFST {}", ch.offset))?;
    session.write(&format!(
        "BWL C{n},{}",
        if ch.bandwidth_hz <= 20e6 { "ON" } else { "OFF" }
    ))?;
    session.write(&format!(
        "C{n}:TRA {}",
        if ch.enabled { "ON" } else { "OFF" }
    ))
}

fn dummy_channel() -> ChannelConfig {
    ChannelConfig {
        enabled: false,
        scale: 1.0,
        position: 0.0,
        offset: 0.0,
        coupling: "DC".into(),
        termination_ohms: 1e6,
        bandwidth_hz: 100e6,
        probe_gain: 1.0,
        probe_type: "unused".into(),
        wave_type: String::new(),
        frequency_hz: 0.0,
    }
}

/// `TRSE?` answers e.g. `EDGE,SR,C1,HT,OFF`; the source is always the third
/// comma field.
fn trse_source(resp: &str) -> String {
    let body = resp.trim().trim_start_matches("TRSE").trim();
    body.split(',')
        .nth(2)
        .map(|s| s.trim().to_ascii_uppercase())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "C1".into())
}

fn trmd_mode(resp: &str) -> String {
    clean_token(resp)
}

fn clean_token(resp: &str) -> String {
    resp.trim()
        .rsplit(' ')
        .next()
        .unwrap_or("")
        .replace('\'', "")
        .to_ascii_uppercase()
}

/// `MSIZ?` answers a discrete size like `14M`, `700K`, or `7K`.
fn parse_msiz(resp: &str) -> Result<u64, ScpiError> {
    let body = resp.trim().trim_start_matches("MSIZ").trim();
    let (mantissa, factor) = if let Some(prefix) = body.strip_suffix(['K', 'k']) {
        (prefix, 1_000.0)
    } else if let Some(prefix) = body.strip_suffix(['M', 'm']) {
        (prefix, 1_000_000.0)
    } else {
        (body, 1.0)
    };
    let value: f64 = mantissa
        .trim()
        .parse()
        .map_err(|_| ScpiError::Parse(format!("MSIZ? returned {resp:?}")))?;
    Ok((value * factor).round() as u64)
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_sds1104x_e_from_idn() {
        let s = Sds::from_idn("Siglent Technologies,SDS1104X-E,SDS1EBAC0L0098,7.6.1.15");
        assert_eq!(s.name, "Siglent SDS1104X-E");
        assert_eq!(s.channel_count, 4);
        assert_eq!(s.bandwidth_hz, 100e6);
    }

    #[test]
    fn handles_idn_without_hyphen() {
        // *IDN? on real hardware answers "SDS1204XE", no hyphen, per the
        // official programming guide's own worked example.
        let s = Sds::from_idn("Siglent Technologies,SDS1204XE,SN,7.6.1.15");
        assert_eq!(s.channel_count, 4);
        assert_eq!(s.bandwidth_hz, 200e6);
    }

    #[test]
    fn two_channel_models() {
        assert_eq!(Sds::from_idn("Siglent,SDS1102X-E,SN,1").channel_count, 2);
        assert_eq!(Sds::from_idn("Siglent,SDS1202XE,SN,1").channel_count, 2);
    }

    #[test]
    fn converts_code_to_voltage_per_manual_worked_example() {
        // PG01-E02D p.264-265 worked example: vdiv=0.5V, voffset=-0.5V, code=2 -> 0.54V.
        let vdiv = 0.5;
        let offset = -0.5;
        let code = 2.0_f64;
        let voltage = code * (vdiv / CODE_PER_DIV) - offset;
        assert!((voltage - 0.54).abs() < 1e-9);
    }

    #[test]
    fn negative_byte_wraps_like_twos_complement() {
        // Manual: byte 0xFC (252 unsigned) is code -4.
        let code = 0xFCu8 as i8 as f64;
        assert_eq!(code, -4.0);
    }

    #[test]
    fn time_axis_matches_manual_worked_example() {
        // PG01-E02D p.265: tdiv=5ns, sample_rate=1GSa/s -> first point -35ns, second -34ns.
        let tdiv = 5.0e-9;
        let sample_rate = 1.0e9;
        let t0 = -(tdiv * GRID_DIVISIONS / 2.0);
        let dt = 1.0 / sample_rate;
        assert!((t0 - (-35e-9)).abs() < 1e-15);
        assert!((t0 + dt - (-34e-9)).abs() < 1e-15);
    }

    #[test]
    fn parses_bwl_query_for_one_channel() {
        // Simulated CHDR-OFF response body (header stripping tested separately).
        let resp = "C1,OFF,C2,ON,C3,OFF,C4,OFF";
        let tokens: Vec<&str> = resp.split(',').map(str::trim).collect();
        let on = tokens
            .iter()
            .position(|t| t.eq_ignore_ascii_case("C2"))
            .and_then(|i| tokens.get(i + 1))
            .is_some_and(|m| m.eq_ignore_ascii_case("ON"));
        assert!(on);
    }

    #[test]
    fn maps_coupling_round_trip() {
        assert_eq!(coupling_from_wire("D1M"), ("DC".into(), 1e6));
        assert_eq!(coupling_from_wire("D50"), ("DC".into(), 50.0));
        assert_eq!(coupling_from_wire("A1M"), ("AC".into(), 1e6));
        assert_eq!(coupling_from_wire("A50"), ("AC".into(), 50.0));
        assert_eq!(coupling_to_wire("DC", 1e6), "D1M");
        assert_eq!(coupling_to_wire("DC", 50.0), "D50");
        assert_eq!(coupling_to_wire("AC", 1e6), "A1M");
        assert_eq!(coupling_to_wire("AC", 50.0), "A50");
    }

    #[test]
    fn parses_msiz_sizes() {
        assert_eq!(parse_msiz("14M").unwrap(), 14_000_000);
        assert_eq!(parse_msiz("700K").unwrap(), 700_000);
        assert_eq!(parse_msiz("1.4M").unwrap(), 1_400_000);
        assert_eq!(parse_msiz("MSIZ 7K").unwrap(), 7_000);
    }

    #[test]
    fn parses_trse_source() {
        assert_eq!(trse_source("EDGE,SR,C1,HT,OFF"), "C1");
        assert_eq!(trse_source("TRSE EDGE,SR,C2,HT,TI,HV,1.43E-06"), "C2");
    }

    #[test]
    fn cleans_sast_apostrophe() {
        assert_eq!(clean_token("SAST Trig'd"), "TRIGD");
        assert_eq!(clean_token("Stop"), "STOP");
    }
}
