use std::f64::consts::PI;
use std::time::Duration;

use crate::backend::{
    channel_number, AcquisitionStatus, Backend, InstrumentCapabilities, InstrumentKind, ValueChoice,
};
use crate::config::{
    query_bool, query_f64, ChannelConfig, HorizontalConfig, InstrumentConfig, TriggerConfig,
};
use crate::scpi::{parse_f64, ScpiError, ScpiSession};
use crate::waveform::{ChannelTrace, WaveformError};

/// Tektronix AFG3000/3000B/3000C series function generator, covering the
/// AFG3051C. Command surface is from the AFG3000 Series Programmer Manual:
/// socket port 4000, `OUTPut<n>:STATe` / `:IMPedance`,
/// `SOURce<n>:FUNCtion:SHAPe`, `SOURce<n>:FREQuency`,
/// `SOURce<n>:VOLTage:LEVel:IMMediate:AMPLitude` / `:OFFSet`.
///
/// This is not an oscilloscope. Fetch plots a preview of the programmed basic
/// wave; it does not digitise the output.
pub struct Afg {
    name: String,
    channel_count: usize,
    max_hz: f64,
}

impl Afg {
    pub fn from_idn(idn: &str) -> Self {
        let model = idn
            .split(',')
            .nth(1)
            .unwrap_or("")
            .trim()
            .to_ascii_uppercase();
        Self {
            name: if model.is_empty() {
                "Tektronix AFG".into()
            } else {
                format!("Tektronix {model}")
            },
            channel_count: channels_for_model(&model),
            max_hz: max_hz_for_model(&model),
        }
    }
}

/// The AFG3000 family names single-channel models with an odd trailing digit
/// (3011, 3021, 3101, 3251, …) and dual-channel models with an even one
/// (3022, 3102, 3252, …). Unrecognised numbers fall back to one channel.
fn channels_for_model(model: &str) -> usize {
    let digits: String = model.chars().filter(|c| c.is_ascii_digit()).collect();
    match digits.chars().last().and_then(|c| c.to_digit(10)) {
        Some(d) if d % 2 == 0 && d != 0 => 2,
        _ => 1,
    }
}

/// Best-effort ceiling for the capability list's default entry. The real
/// limit is queried from the instrument at connect time via `bandwidth_hz`
/// and folded into the list, so a wrong guess here is only ever a cosmetic
/// starting value.
fn max_hz_for_model(model: &str) -> f64 {
    const TABLE: &[(&str, f64)] = &[
        ("3011", 10e6),
        ("3021", 25e6),
        ("3022", 25e6),
        ("3101", 100e6),
        ("3102", 100e6),
        ("3251", 240e6),
        ("3252", 240e6),
    ];
    TABLE
        .iter()
        .find(|(code, _)| model.contains(code))
        .map(|(_, hz)| *hz)
        .unwrap_or(25e6)
}

impl Backend for Afg {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> InstrumentKind {
        InstrumentKind::Generator
    }

    fn capabilities(&self) -> InstrumentCapabilities {
        InstrumentCapabilities {
            channel_couplings: vec!["DC".into()],
            terminations: vec![ValueChoice::new("50 Ω", 50.0), ValueChoice::new("HiZ", 1e6)],
            termination_writable: true,
            bandwidths: vec![ValueChoice::new(
                format!("Max sine ({:.0} MHz)", self.max_hz / 1e6),
                self.max_hz,
            )],
            record_lengths: vec![2_000],
            trigger_modes: vec!["NONE".into()],
            trigger_slopes: vec!["RISE".into()],
            trigger_couplings: vec!["DC".into()],
            acquisition_modes: vec!["PREVIEW".into()],
            stop_after: vec!["RUNSTOP".into()],
            channel_hint: Some(
                "AFG3000 output load is 50 Ω or HiZ. Fetch draws a preview of the programmed wave."
                    .into(),
            ),
            horizontal_hint: Some("Timebase follows two periods of the selected wave.".into()),
            acquisition_hint: Some(
                "This generator does not acquire; Fetch plots a preview.".into(),
            ),
            kind: InstrumentKind::Generator,
            channel_count: self.channel_count,
            wave_types: strings(&["SINE", "SQUARE", "RAMP", "PULSE", "NOISE", "DC"]),
            output_pairs: Vec::new(),
        }
    }

    fn channel_enabled(&self, s: &mut ScpiSession, n: usize) -> Result<bool, ScpiError> {
        query_bool(s, &format!("OUTP{n}:STAT?"))
    }

    fn set_channel_enabled(
        &self,
        s: &mut ScpiSession,
        n: usize,
        on: bool,
    ) -> Result<(), ScpiError> {
        s.write(&format!("OUTP{n}:STAT {}", if on { "ON" } else { "OFF" }))
    }

    fn termination_ohms(&self, s: &mut ScpiSession, n: usize) -> Result<f64, ScpiError> {
        parse_ohms(&s.query(&format!("OUTP{n}:IMP?"))?)
    }

    fn set_termination_ohms(
        &self,
        s: &mut ScpiSession,
        n: usize,
        ohms: f64,
    ) -> Result<(), ScpiError> {
        if ohms >= 1e5 {
            s.write(&format!("OUTP{n}:IMP INF"))
        } else {
            s.write(&format!("OUTP{n}:IMP {ohms}"))
        }
    }

    /// Queried live rather than guessed from the model number: `SOURce<n>:
    /// FREQuency? MAX` reports this instrument's actual sine ceiling.
    fn bandwidth_hz(&self, s: &mut ScpiSession, n: usize) -> Result<f64, ScpiError> {
        query_f64(s, &format!("SOUR{n}:FREQ? MAX"))
    }

    fn set_bandwidth_hz(&self, _s: &mut ScpiSession, _n: usize, _hz: f64) -> Result<(), ScpiError> {
        Ok(())
    }

    fn probe_type(&self, _s: &mut ScpiSession, _n: usize) -> Result<String, ScpiError> {
        Ok("generator".into())
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

    fn fetch_channel(&self, s: &mut ScpiSession, ch: &str) -> Result<ChannelTrace, WaveformError> {
        let n = channel_number(ch)
            .ok_or_else(|| WaveformError::Parse(format!("AFG channel {ch} is not CH1/CH2")))?;
        if n > self.channel_count {
            return Err(WaveformError::Parse(format!(
                "{} has {} channel(s)",
                self.name, self.channel_count
            )));
        }
        let wave = read_wave(s, n)?;
        Ok(preview_trace(&format!("CH{n}"), &wave))
    }

    fn wait_sequence(&self, _s: &mut ScpiSession, _timeout: Duration) -> Result<(), ScpiError> {
        Ok(())
    }

    fn acquisition_status(&self, s: &mut ScpiSession) -> Result<AcquisitionStatus, ScpiError> {
        let mut on = Vec::with_capacity(self.channel_count);
        for n in 1..=self.channel_count {
            on.push(self.channel_enabled(s, n)?);
        }
        Ok(AcquisitionStatus {
            running: on.iter().any(|&x| x),
            display: on
                .iter()
                .enumerate()
                .map(|(i, &x)| format!("C{} {}", i + 1, if x { "OUT" } else { "OFF" }))
                .collect::<Vec<_>>()
                .join("  "),
        })
    }

    fn autoset(&self, _s: &mut ScpiSession) -> Result<(), ScpiError> {
        Err(ScpiError::Unsupported(
            "AFG has no autoset; set frequency and amplitude on the channel.".into(),
        ))
    }
}

pub fn read_config(
    session: &mut ScpiSession,
    backend: &dyn Backend,
) -> Result<InstrumentConfig, ScpiError> {
    let nch = backend.capabilities().channel_count;
    let mut channels = vec![dummy_channel(); nch];
    for n in 1..=nch {
        channels[n - 1] = read_afg_channel(session, backend, n)?;
    }
    let freq = channels
        .iter()
        .take(nch)
        .find(|ch| ch.enabled && ch.frequency_hz > 0.0)
        .map(|ch| ch.frequency_hz)
        .unwrap_or(channels[0].frequency_hz)
        .max(1.0);
    let running = channels.iter().take(nch).any(|ch| ch.enabled);
    Ok(InstrumentConfig {
        channels,
        horizontal: HorizontalConfig {
            scale: 2.0 / freq / 10.0,
            position: 50.0,
            record_length: 2_000,
        },
        trigger: TriggerConfig {
            mode: "NONE".into(),
            source: "CH1".into(),
            slope: "RISE".into(),
            coupling: "DC".into(),
            level: 0.0,
        },
        acquisition: crate::config::AcquisitionConfig {
            mode: "PREVIEW".into(),
            stop_after: "RUNSTOP".into(),
            running,
        },
        output_pair: String::new(),
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
    let shape = wave_type_to_shape(&ch.wave_type);
    session.write(&format!("SOUR{n}:FUNC:SHAP {shape}"))?;
    if shape == "DC" {
        session.write(&format!("SOUR{n}:VOLT:OFFS {}", ch.offset))?;
    } else {
        session.write(&format!("SOUR{n}:FREQ {}", ch.frequency_hz.max(1e-3)))?;
        session.write(&format!("SOUR{n}:VOLT:AMPL {}", ch.scale.max(0.001)))?;
        session.write(&format!("SOUR{n}:VOLT:OFFS {}", ch.offset))?;
    }
    if ch.termination_ohms >= 1e5 {
        session.write(&format!("OUTP{n}:IMP INF"))?;
    } else {
        session.write(&format!("OUTP{n}:IMP {}", ch.termination_ohms))?;
    }
    session.write(&format!(
        "OUTP{n}:STAT {}",
        if ch.enabled { "ON" } else { "OFF" }
    ))?;
    Ok(())
}

fn read_afg_channel(
    session: &mut ScpiSession,
    backend: &dyn Backend,
    n: usize,
) -> Result<ChannelConfig, ScpiError> {
    let enabled = backend.channel_enabled(session, n)?;
    let wave = read_wave(session, n)?;
    Ok(ChannelConfig {
        enabled,
        scale: wave.ampl,
        position: 0.0,
        offset: wave.offs,
        coupling: "DC".into(),
        termination_ohms: backend.termination_ohms(session, n)?,
        bandwidth_hz: backend.bandwidth_hz(session, n)?,
        probe_gain: 1.0,
        probe_type: "generator".into(),
        wave_type: wave.shape.clone(),
        frequency_hz: wave.freq,
    })
}

fn dummy_channel() -> ChannelConfig {
    ChannelConfig {
        enabled: false,
        scale: 1.0,
        position: 0.0,
        offset: 0.0,
        coupling: "DC".into(),
        termination_ohms: 1e6,
        bandwidth_hz: 25e6,
        probe_gain: 1.0,
        probe_type: "unused".into(),
        wave_type: "SINE".into(),
        frequency_hz: 1e3,
    }
}

#[derive(Debug, Clone)]
struct Wave {
    shape: String,
    freq: f64,
    ampl: f64,
    offs: f64,
}

fn read_wave(session: &mut ScpiSession, n: usize) -> Result<Wave, ScpiError> {
    let shape = shape_to_wave_type(&session.query(&format!("SOUR{n}:FUNC:SHAP?"))?);
    let freq = query_f64(session, &format!("SOUR{n}:FREQ?"))?;
    let offs = query_f64(session, &format!("SOUR{n}:VOLT:OFFS?"))?;
    let ampl = if shape == "DC" {
        0.0
    } else {
        query_f64(session, &format!("SOUR{n}:VOLT:AMPL?"))?
    };
    Ok(Wave {
        shape,
        freq,
        ampl,
        offs,
    })
}

/// `SOURce<n>:FUNCtion:SHAPe?` short forms map onto the display vocabulary
/// shared with the rest of the UI; anything unrecognised (`SINC`, `GAUS`,
/// `USER1`, …) is passed through unchanged.
fn shape_to_wave_type(resp: &str) -> String {
    let upper = resp.trim().trim_matches('"').to_ascii_uppercase();
    match upper.as_str() {
        "SIN" | "SINUSOID" => "SINE".into(),
        "SQU" | "SQUARE" => "SQUARE".into(),
        "RAMP" => "RAMP".into(),
        "PULS" | "PULSE" => "PULSE".into(),
        "PRN" | "PRNOISE" | "NOISE" => "NOISE".into(),
        "DC" => "DC".into(),
        other => other.to_string(),
    }
}

fn wave_type_to_shape(value: &str) -> &'static str {
    match value.to_ascii_uppercase().as_str() {
        "SQUARE" => "SQU",
        "RAMP" => "RAMP",
        "PULSE" => "PULS",
        "NOISE" => "PRN",
        "DC" => "DC",
        _ => "SIN",
    }
}

fn parse_ohms(resp: &str) -> Result<f64, ScpiError> {
    let trimmed = resp.trim();
    if trimmed.to_ascii_uppercase().contains("INF") {
        return Ok(1e6);
    }
    let value = parse_f64(trimmed)?;
    Ok(if value > 1e5 { 1e6 } else { value })
}

fn preview_trace(ch: &str, wave: &Wave) -> ChannelTrace {
    let n = 2000usize;
    let freq = if wave.freq > 0.0 { wave.freq } else { 1e3 };
    let span = if wave.shape == "DC" { 1e-3 } else { 2.0 / freq };
    let dt = span / n as f64;
    let amp = wave.ampl / 2.0;
    let mut seed = 0x5A5Au32;
    let points = (0..n)
        .map(|i| {
            let t = i as f64 * dt;
            let v = match wave.shape.as_str() {
                "DC" => wave.offs,
                "NOISE" => {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    let u = (seed >> 8) as f64 / (1u32 << 24) as f64;
                    wave.offs + amp * (2.0 * u - 1.0)
                }
                "SQUARE" => {
                    let p = (t * freq).rem_euclid(1.0);
                    wave.offs + if p < 0.5 { amp } else { -amp }
                }
                "PULSE" => {
                    let p = (t * freq).rem_euclid(1.0);
                    wave.offs + if p < 0.2 { amp } else { -amp }
                }
                "RAMP" => {
                    let p = (t * freq).rem_euclid(1.0);
                    wave.offs + amp * (2.0 * p - 1.0)
                }
                _ => wave.offs + amp * (2.0 * PI * freq * t).sin(),
            };
            [t, v]
        })
        .collect();
    ChannelTrace {
        channel: ch.to_string(),
        x_unit: "s".into(),
        y_unit: "V".into(),
        points,
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|v| (*v).to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_afg3051c_from_idn() {
        let a = Afg::from_idn("TEKTRONIX,AFG3051C,SN,SCPI:99.0 FV:2.7");
        assert_eq!(a.name, "Tektronix AFG3051C");
        assert_eq!(a.channel_count, 1);
    }

    #[test]
    fn even_trailing_digit_is_two_channels() {
        assert_eq!(channels_for_model("AFG3252C"), 2);
        assert_eq!(channels_for_model("AFG3101"), 1);
        assert_eq!(channels_for_model("AFG3022"), 2);
    }

    #[test]
    fn known_models_get_a_max_frequency() {
        assert_eq!(max_hz_for_model("AFG3251C"), 240e6);
        assert_eq!(max_hz_for_model("AFG3051C"), 25e6);
    }

    #[test]
    fn maps_shape_round_trip() {
        assert_eq!(shape_to_wave_type("SIN"), "SINE");
        assert_eq!(shape_to_wave_type("SQU"), "SQUARE");
        assert_eq!(shape_to_wave_type("PRN"), "NOISE");
        assert_eq!(wave_type_to_shape("SQUARE"), "SQU");
        assert_eq!(wave_type_to_shape("NOISE"), "PRN");
        assert_eq!(wave_type_to_shape("SINE"), "SIN");
    }

    #[test]
    fn parses_impedance_query() {
        assert_eq!(parse_ohms("50").unwrap(), 50.0);
        assert_eq!(parse_ohms("5.0E+01").unwrap(), 50.0);
        assert_eq!(parse_ohms("9.9E+37").unwrap(), 1e6);
        assert_eq!(parse_ohms("INF").unwrap(), 1e6);
    }

    #[test]
    fn preview_has_two_thousand_points() {
        let wave = Wave {
            shape: "SINE".into(),
            freq: 1e3,
            ampl: 2.0,
            offs: 0.0,
        };
        let trace = preview_trace("CH1", &wave);
        assert_eq!(trace.points.len(), 2000);
        assert!(trace.points[0][1].abs() < 1e-9);
    }
}
