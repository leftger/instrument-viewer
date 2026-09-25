use std::time::Duration;

use crate::backend::{
    channel_number, AcquisitionStatus, Backend, InstrumentCapabilities, InstrumentKind, ValueChoice,
};
use crate::config::{
    query_bool, query_f64, ChannelConfig, HorizontalConfig, InstrumentConfig, TriggerConfig,
};
use crate::scpi::{parse_f64, ScpiError, ScpiSession};
use crate::waveform::{ChannelTrace, WaveformError};

/// Keysight/Agilent E36xx bench supplies on LAN socket **5025**.
///
/// E36200 programming is `INST:NSEL` (multi-output models only), `VOLT`,
/// `CURR`, `OUTP`, `MEAS:VOLT?`, and `MEAS:CURR?`. The E36233A is two
/// autoranged 30 V / 20 A outputs, each limited to 200 W delivered. Output 2
/// is selected with `INST:NSEL 2`. `OUTP:PAIR` ties the two outputs in series
/// or parallel; output 2 then follows output 1. Verified against an E36233A
/// (firmware 1.1.1-1.0.3-1.01).
///
/// The single-output E36231A answers `INST:NSEL` with an undefined header, so
/// that command is sent only when there is more than one output.
///
/// The older E3631A triple-output (P6V / P25V / N25V) uses the same
/// `APPLy` / `INST:SEL` / `OUTP` pattern as
/// <https://github.com/psmd-iberutaru/Keysight-E3631A-Python>.
pub struct KeysightPsu {
    name: String,
    profile: Profile,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Profile {
    dialect: Dialect,
    outputs: &'static [Output],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialect {
    /// E36200: `INST:NSEL n`, `VOLT`, `CURR`, `OUTP`, `MEAS:*?`.
    E36200,
    /// E3631A: `INST:SEL P6V|P25V|N25V`, `APPLy`, global `OUTP`.
    E3631,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Output {
    label: &'static str,
    inst: &'static str,
    min_v: f64,
    max_v: f64,
    max_a: f64,
    /// Maximum power the output can deliver. Programming may accept a
    /// voltage/current pair outside this; the output folds back at the limit.
    max_w: f64,
}

const E36231: &[Output] = &[Output {
    label: "CH1",
    inst: "1",
    min_v: 0.0,
    max_v: 30.9,
    max_a: 20.6,
    max_w: 200.0,
}];

const E36232: &[Output] = &[Output {
    label: "CH1",
    inst: "1",
    min_v: 0.0,
    max_v: 61.8,
    max_a: 10.3,
    max_w: 200.0,
}];

const E36233: &[Output] = &[
    Output {
        label: "CH1",
        inst: "1",
        min_v: 0.0,
        max_v: 30.9,
        max_a: 20.6,
        max_w: 200.0,
    },
    Output {
        label: "CH2",
        inst: "2",
        min_v: 0.0,
        max_v: 30.9,
        max_a: 20.6,
        max_w: 200.0,
    },
];

const E36234: &[Output] = &[
    Output {
        label: "CH1",
        inst: "1",
        min_v: 0.0,
        max_v: 61.8,
        max_a: 10.3,
        max_w: 200.0,
    },
    Output {
        label: "CH2",
        inst: "2",
        min_v: 0.0,
        max_v: 61.8,
        max_a: 10.3,
        max_w: 200.0,
    },
];

const E3631: &[Output] = &[
    Output {
        label: "P6V",
        inst: "P6V",
        min_v: 0.0,
        max_v: 6.0,
        max_a: 5.0,
        max_w: 30.0,
    },
    Output {
        label: "P25V",
        inst: "P25V",
        min_v: 0.0,
        max_v: 25.0,
        max_a: 1.0,
        max_w: 25.0,
    },
    Output {
        label: "N25V",
        inst: "N25V",
        min_v: -25.0,
        max_v: 0.0,
        max_a: 1.0,
        max_w: 25.0,
    },
];

const E3633: &[Output] = &[Output {
    label: "CH1",
    inst: "1",
    min_v: 0.0,
    max_v: 20.0,
    max_a: 10.0,
    max_w: 200.0,
}];

pub fn is_power_supply_idn(idn: &str) -> bool {
    let upper = idn.to_ascii_uppercase();
    const MODELS: &[&str] = &[
        "E36231", "E36232", "E36233", "E36234", "E3631", "E3633", "E3634",
    ];
    MODELS.iter().any(|model| upper.contains(model))
}

impl KeysightPsu {
    pub fn from_idn(idn: &str) -> Self {
        let model = idn
            .split(',')
            .nth(1)
            .unwrap_or("")
            .trim()
            .to_ascii_uppercase();
        let profile = profile_for_model(&model);
        Self {
            name: if model.is_empty() {
                "Keysight PSU".into()
            } else {
                format!("Keysight {model}")
            },
            profile,
        }
    }

    fn output(&self, n: usize) -> Result<&Output, ScpiError> {
        self.profile
            .outputs
            .get(n.wrapping_sub(1))
            .ok_or_else(|| ScpiError::Unsupported(format!("PSU has no output {n}")))
    }

    fn select(&self, s: &mut ScpiSession, n: usize) -> Result<&Output, ScpiError> {
        let out = self.output(n)?;
        if let Some(command) = select_command(&self.profile, out) {
            s.write(&command)?;
        }
        Ok(out)
    }

    fn measure(&self, s: &mut ScpiSession, n: usize) -> Result<(f64, f64), ScpiError> {
        self.select(s, n)?;
        let v = query_f64(s, "MEAS:VOLT?")?;
        let i = query_f64(s, "MEAS:CURR?")?;
        Ok((v, i))
    }
}

/// Single-output supplies have no `INSTrument` subsystem: an E36231A answers
/// `INST:NSEL 1`, `INST?` and `INST:SEL?` with `-113,"Undefined header"` and
/// beeps `!Err`. Only the multi-output models are selectable.
fn select_command(profile: &Profile, out: &Output) -> Option<String> {
    if profile.outputs.len() < 2 {
        return None;
    }
    Some(match profile.dialect {
        Dialect::E36200 => format!("INST:NSEL {}", out.inst),
        Dialect::E3631 => format!("INST:SEL {}", out.inst),
    })
}

fn supports_pairing(profile: &Profile) -> bool {
    profile.dialect == Dialect::E36200 && profile.outputs.len() >= 2
}

fn output_of(profile: &Profile, n: usize) -> Result<&'static Output, ScpiError> {
    let outputs: &'static [Output] = profile.outputs;
    outputs
        .get(n.wrapping_sub(1))
        .ok_or_else(|| ScpiError::Unsupported(format!("PSU has no output {n}")))
}

/// `OUTP:PAIR?` short forms are `OFF`, `PAR`, and `SER`.
fn canonical_pair(text: &str) -> &'static str {
    let upper = text.trim().trim_matches('"').to_ascii_uppercase();
    if upper.starts_with("PAR") {
        "PARALLEL"
    } else if upper.starts_with("SER") {
        "SERIES"
    } else {
        "OFF"
    }
}

/// `Some(mode)` when output 2 is slaved to output 1.
fn paired_mode(
    session: &mut ScpiSession,
    profile: &Profile,
) -> Result<Option<&'static str>, ScpiError> {
    if !supports_pairing(profile) {
        return Ok(None);
    }
    let mode = canonical_pair(&session.query("OUTP:PAIR?")?);
    if mode == "OFF" {
        Ok(None)
    } else {
        Ok(Some(mode))
    }
}

fn check_supply_error(session: &mut ScpiSession) -> Result<(), ScpiError> {
    let err = session.query("SYST:ERR?")?;
    let trimmed = err.trim();
    if trimmed.starts_with("+0") || trimmed.starts_with("0,") || trimmed == "0" {
        Ok(())
    } else {
        Err(ScpiError::Unsupported(format!(
            "supply rejected the setting: {err}"
        )))
    }
}

pub fn read_output_pair(
    session: &mut ScpiSession,
    backend: &dyn Backend,
) -> Result<String, ScpiError> {
    let profile = profile_for_model(backend.name());
    if !supports_pairing(&profile) {
        return Ok(String::new());
    }
    Ok(canonical_pair(&session.query("OUTP:PAIR?")?).to_string())
}

pub fn apply_output_pair(
    session: &mut ScpiSession,
    backend: &dyn Backend,
    mode: &str,
) -> Result<(), ScpiError> {
    let profile = profile_for_model(backend.name());
    if !supports_pairing(&profile) {
        return Err(ScpiError::Unsupported(
            "this supply has no series/parallel pairing".into(),
        ));
    }
    let mode = canonical_pair(mode);
    session.write("*CLS")?;
    session.write(&format!("OUTP:PAIR {mode}"))?;
    check_supply_error(session)
}

fn profile_for_model(model: &str) -> Profile {
    let model = model.to_ascii_uppercase();
    if model.contains("E3631") {
        Profile {
            dialect: Dialect::E3631,
            outputs: E3631,
        }
    } else if model.contains("E36234") {
        Profile {
            dialect: Dialect::E36200,
            outputs: E36234,
        }
    } else if model.contains("E36233") {
        Profile {
            dialect: Dialect::E36200,
            outputs: E36233,
        }
    } else if model.contains("E36232") {
        Profile {
            dialect: Dialect::E36200,
            outputs: E36232,
        }
    } else if model.contains("E3633") || model.contains("E3634") {
        Profile {
            dialect: Dialect::E36200,
            outputs: E3633,
        }
    } else {
        Profile {
            dialect: Dialect::E36200,
            outputs: E36231,
        }
    }
}

impl Backend for KeysightPsu {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> InstrumentKind {
        InstrumentKind::Supply
    }

    fn preamble(&self) -> Vec<String> {
        if self.profile.dialect == Dialect::E3631 {
            vec!["SYST:REM".into()]
        } else {
            Vec::new()
        }
    }

    fn capabilities(&self) -> InstrumentCapabilities {
        let n = self.profile.outputs.len();
        let max_v = self
            .profile
            .outputs
            .iter()
            .map(|o| o.max_v.abs())
            .fold(0.0, f64::max);
        let max_a = self
            .profile
            .outputs
            .iter()
            .map(|o| o.max_a)
            .fold(0.0, f64::max);
        let max_w = self
            .profile
            .outputs
            .iter()
            .map(|o| o.max_w)
            .fold(0.0, f64::max);
        let names: String = self
            .profile
            .outputs
            .iter()
            .map(|o| o.label)
            .collect::<Vec<_>>()
            .join(", ");
        let pairing = if supports_pairing(&self.profile) {
            " Series stacks the voltages; parallel stacks the currents; output 2 follows output 1."
        } else {
            ""
        };
        InstrumentCapabilities {
            channel_couplings: vec!["DC".into()],
            terminations: vec![ValueChoice::new("fixed", 1e6)],
            termination_writable: false,
            bandwidths: vec![ValueChoice::new("DC", 0.0)],
            record_lengths: vec![200],
            trigger_modes: vec!["NONE".into()],
            trigger_slopes: vec!["RISE".into()],
            trigger_couplings: vec!["DC".into()],
            acquisition_modes: vec!["MEASURE".into()],
            stop_after: vec!["RUNSTOP".into()],
            channel_hint: Some(format!(
                "{names}. Voltage setpoint and current limit. Fetch reads one voltage and current sample per output. A second reading in the same session starts the time plot. Programming max {max_v} V / {max_a} A; delivered power is limited to {max_w} W per output.{pairing}"
            )),
            horizontal_hint: Some(
                "Each Fetch is one instant. Time on the plot is seconds since the first reading in this session.".into(),
            ),
            acquisition_hint: Some(
                "This power supply does not acquire waveforms. Fetch reads MEAS:VOLT? and MEAS:CURR? once.".into(),
            ),
            kind: InstrumentKind::Supply,
            channel_count: n,
            wave_types: Vec::new(),
            output_pairs: if supports_pairing(&self.profile) {
                vec!["OFF".into(), "PARALLEL".into(), "SERIES".into()]
            } else {
                Vec::new()
            },
        }
    }

    fn channel_enabled(&self, s: &mut ScpiSession, n: usize) -> Result<bool, ScpiError> {
        self.select(s, n)?;
        query_bool(s, "OUTP?")
    }

    fn set_channel_enabled(
        &self,
        s: &mut ScpiSession,
        n: usize,
        on: bool,
    ) -> Result<(), ScpiError> {
        self.select(s, n)?;
        s.write(&format!("OUTP {}", if on { "ON" } else { "OFF" }))
    }

    fn termination_ohms(&self, _s: &mut ScpiSession, _n: usize) -> Result<f64, ScpiError> {
        Ok(1e6)
    }

    fn set_termination_ohms(
        &self,
        _s: &mut ScpiSession,
        _n: usize,
        _ohms: f64,
    ) -> Result<(), ScpiError> {
        Ok(())
    }

    fn bandwidth_hz(&self, _s: &mut ScpiSession, _n: usize) -> Result<f64, ScpiError> {
        Ok(0.0)
    }

    fn set_bandwidth_hz(&self, _s: &mut ScpiSession, _n: usize, _hz: f64) -> Result<(), ScpiError> {
        Ok(())
    }

    fn probe_type(&self, s: &mut ScpiSession, n: usize) -> Result<String, ScpiError> {
        let (v, i) = self.measure(s, n)?;
        let label = self.output(n)?.label;
        Ok(format!("{label} meas {v:.4} V / {i:.4} A"))
    }

    fn trigger_level(&self, _s: &mut ScpiSession, _source: &str) -> Result<f64, ScpiError> {
        Ok(0.0)
    }

    fn set_trigger_level(
        &self,
        _s: &mut ScpiSession,
        _source: &str,
        _volts: f64,
    ) -> Result<(), ScpiError> {
        Ok(())
    }

    fn apply_acquisition(
        &self,
        _s: &mut ScpiSession,
        _mode: &str,
        _stop_after: &str,
        _running: bool,
    ) -> Result<(), ScpiError> {
        Ok(())
    }

    fn fetch_channel(&self, s: &mut ScpiSession, ch: &str) -> Result<ChannelTrace, WaveformError> {
        let n = channel_number(ch)
            .ok_or_else(|| WaveformError::Parse(format!("PSU channel {ch} is not CH1/CH2/CH3")))?;
        let (v, _i) = self.measure(s, n)?;
        Ok(dc_trace(&format!("CH{n} Voltage"), "V", v))
    }

    fn fetch_channels(
        &self,
        s: &mut ScpiSession,
        channels: &[String],
    ) -> Result<Vec<ChannelTrace>, WaveformError> {
        let mut traces = Vec::with_capacity(channels.len() * 2);
        for ch in channels {
            let n = channel_number(ch).ok_or_else(|| {
                WaveformError::Parse(format!("PSU channel {ch} is not CH1/CH2/CH3"))
            })?;
            let (volts, amps) = self.measure(s, n)?;
            traces.push(dc_trace(&format!("CH{n} Voltage"), "V", volts));
            traces.push(dc_trace(&format!("CH{n} Current"), "A", amps));
        }
        Ok(traces)
    }

    fn wait_sequence(&self, _s: &mut ScpiSession, _timeout: Duration) -> Result<(), ScpiError> {
        Ok(())
    }

    fn acquisition_status(&self, s: &mut ScpiSession) -> Result<AcquisitionStatus, ScpiError> {
        let mut parts = Vec::new();
        let mut any_on = false;
        for n in 1..=self.profile.outputs.len() {
            let on = self.channel_enabled(s, n)?;
            any_on |= on;
            let (v, i) = self.measure(s, n)?;
            let label = self.output(n)?.label;
            parts.push(format!(
                "{label} {} {v:.3} V {i:.3} A",
                if on { "ON" } else { "OFF" }
            ));
        }
        Ok(AcquisitionStatus {
            running: any_on,
            display: parts.join("  "),
        })
    }

    fn autoset(&self, _s: &mut ScpiSession) -> Result<(), ScpiError> {
        Err(ScpiError::Unsupported(
            "PSU has no autoset; set voltage and current limit on the channel.".into(),
        ))
    }
}

pub fn read_config(
    session: &mut ScpiSession,
    backend: &dyn Backend,
) -> Result<InstrumentConfig, ScpiError> {
    let nch = backend.capabilities().channel_count;
    let mut channels = [
        dummy_channel(),
        dummy_channel(),
        dummy_channel(),
        dummy_channel(),
    ];
    for n in 1..=nch {
        channels[n - 1] = read_output(session, backend, n)?;
    }
    let running = channels.iter().take(nch).any(|ch| ch.enabled);
    Ok(InstrumentConfig {
        channels,
        horizontal: HorizontalConfig {
            scale: 0.1,
            position: 50.0,
            record_length: 200,
        },
        trigger: TriggerConfig {
            mode: "NONE".into(),
            source: "CH1".into(),
            slope: "RISE".into(),
            coupling: "DC".into(),
            level: 0.0,
        },
        acquisition: crate::config::AcquisitionConfig {
            mode: "MEASURE".into(),
            stop_after: "RUNSTOP".into(),
            running,
        },
        output_pair: read_output_pair(session, backend)?,
    })
}

pub fn apply_channel(
    session: &mut ScpiSession,
    backend: &dyn Backend,
    n: usize,
    ch: &ChannelConfig,
) -> Result<(), ScpiError> {
    let profile = profile_for_model(backend.name());
    let out = output_of(&profile, n)?;
    session.write("*CLS")?;
    if let Some(command) = select_command(&profile, out) {
        session.write(&command)?;
    }
    if let Some(pair) = paired_mode(session, &profile)? {
        if n > 1 {
            return Err(ScpiError::Unsupported(format!(
                "output {n} follows output 1 while outputs are in {pair} mode"
            )));
        }
    }
    let (min_v, max_v, max_a) = match profile.dialect {
        Dialect::E36200 => (
            query_f64(session, "VOLT? MIN")?,
            query_f64(session, "VOLT? MAX")?,
            query_f64(session, "CURR? MAX")?,
        ),
        Dialect::E3631 => (out.min_v, out.max_v, out.max_a),
    };
    let volt = ch.scale.clamp(min_v, max_v);
    let curr = ch.offset.abs().clamp(0.0, max_a);
    match profile.dialect {
        Dialect::E3631 => session.write(&format!("APPL {},{volt},{curr}", out.inst))?,
        Dialect::E36200 => {
            session.write(&format!("VOLT {volt}"))?;
            session.write(&format!("CURR {curr}"))?;
        }
    }
    session.write(&format!("OUTP {}", if ch.enabled { "ON" } else { "OFF" }))?;
    check_supply_error(session)
}

fn read_output(
    session: &mut ScpiSession,
    backend: &dyn Backend,
    n: usize,
) -> Result<ChannelConfig, ScpiError> {
    let enabled = backend.channel_enabled(session, n)?;
    let (scale, offset) = read_setpoints(session, backend, n)?;
    Ok(ChannelConfig {
        enabled,
        scale,
        position: 0.0,
        offset,
        coupling: "DC".into(),
        termination_ohms: 1e6,
        bandwidth_hz: 0.0,
        probe_gain: 1.0,
        probe_type: backend
            .probe_type(session, n)
            .unwrap_or_else(|_| "PSU".into()),
        wave_type: String::new(),
        frequency_hz: 0.0,
    })
}

fn read_setpoints(
    session: &mut ScpiSession,
    backend: &dyn Backend,
    n: usize,
) -> Result<(f64, f64), ScpiError> {
    let profile = profile_for_model(backend.name());
    let out = output_of(&profile, n)?;
    if let Some(command) = select_command(&profile, out) {
        session.write(&command)?;
    }
    if profile.dialect == Dialect::E3631 {
        let resp = session.query(&format!("APPL? {}", out.inst))?;
        return parse_apply_query(&resp);
    }
    let v = query_f64(session, "VOLT?")?;
    let i = query_f64(session, "CURR?")?;
    Ok((v, i))
}

fn parse_apply_query(resp: &str) -> Result<(f64, f64), ScpiError> {
    let nums: Vec<f64> = resp
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter_map(|p| parse_f64(p.trim()).ok())
        .collect();
    match nums.as_slice() {
        [.., v, i] => Ok((*v, *i)),
        [v] => Ok((*v, 0.0)),
        _ => Err(ScpiError::Parse(format!("APPL? {resp:?}"))),
    }
}

fn dummy_channel() -> ChannelConfig {
    ChannelConfig {
        enabled: false,
        scale: 0.0,
        position: 0.0,
        offset: 0.0,
        coupling: "DC".into(),
        termination_ohms: 1e6,
        bandwidth_hz: 0.0,
        probe_gain: 1.0,
        probe_type: "unused".into(),
        wave_type: String::new(),
        frequency_hz: 0.0,
    }
}

fn dc_trace(ch: &str, unit: &str, value: f64) -> ChannelTrace {
    ChannelTrace {
        channel: ch.to_string(),
        x_unit: "s".into(),
        y_unit: unit.into(),
        points: vec![[0.0, value]],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::from_idn;

    #[test]
    fn detects_e36231a_idn() {
        assert!(is_power_supply_idn(
            "Keysight Technologies,E36231A,MY00000000,A.02.01.1631"
        ));
        assert!(is_power_supply_idn("HEWLETT-PACKARD,E3631A,0,1.0"));
        assert!(!is_power_supply_idn(
            "Keysight Technologies,DSOX1204G,CN,1.0"
        ));
        assert!(!is_power_supply_idn(
            "TEKTRONIX,MDO3024,B020857,CF:91.1CT FV:v1.30"
        ));
    }

    #[test]
    fn from_idn_picks_supply_not_tek() {
        let psu = from_idn("Keysight Technologies,E36231A,MY1234,A.02.01.1631");
        assert_eq!(psu.kind(), InstrumentKind::Supply);
        assert_eq!(psu.name(), "Keysight E36231A");
        assert_eq!(psu.capabilities().channel_count, 1);
        let dual = from_idn("Keysight Technologies,E36233A,MY61009393,1.1.1-1.0.3-1.01");
        assert_eq!(dual.capabilities().channel_count, 2);
        assert_eq!(
            dual.capabilities().output_pairs,
            vec![
                "OFF".to_string(),
                "PARALLEL".to_string(),
                "SERIES".to_string()
            ]
        );
        let triple = from_idn("Agilent Technologies,E3631A,0,2.0");
        assert_eq!(triple.kind(), InstrumentKind::Supply);
        assert_eq!(triple.capabilities().channel_count, 3);
    }

    #[test]
    fn only_multi_output_supplies_are_selectable() {
        let single = profile_for_model("E36231A");
        assert_eq!(select_command(&single, &single.outputs[0]), None);
        let dual = profile_for_model("E36233A");
        assert_eq!(
            select_command(&dual, &dual.outputs[1]),
            Some("INST:NSEL 2".into())
        );
        let triple = profile_for_model("E3631A");
        assert_eq!(
            select_command(&triple, &triple.outputs[2]),
            Some("INST:SEL N25V".into())
        );
    }

    #[test]
    fn parses_apply_query() {
        assert_eq!(
            parse_apply_query("+6.00000000E+00,+5.00000000E+00").unwrap(),
            (6.0, 5.0)
        );
        assert_eq!(parse_apply_query("P6V,6.000,5.000").unwrap(), (6.0, 5.0));
    }

    #[test]
    fn pairing_query_forms() {
        assert_eq!(canonical_pair("OFF"), "OFF");
        assert_eq!(canonical_pair("PAR"), "PARALLEL");
        assert_eq!(canonical_pair("PARALLEL"), "PARALLEL");
        assert_eq!(canonical_pair("SER"), "SERIES");
        assert_eq!(canonical_pair("SERIES"), "SERIES");
        assert!(supports_pairing(&profile_for_model("E36233A")));
        assert!(!supports_pairing(&profile_for_model("E36231A")));
        assert!(!supports_pairing(&profile_for_model("E3631A")));
    }

    #[test]
    fn dc_trace_preserves_measurement_unit() {
        let current = dc_trace("CH2 Current", "A", 1.25);
        assert_eq!(current.channel, "CH2 Current");
        assert_eq!(current.y_unit, "A");
        assert_eq!(current.points, vec![[0.0, 1.25]]);
    }
}
