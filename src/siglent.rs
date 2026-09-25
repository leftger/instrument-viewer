use std::collections::HashMap;
use std::f64::consts::PI;
use std::time::Duration;

use crate::backend::{
    channel_number, AcquisitionStatus, Backend, InstrumentCapabilities, InstrumentKind, ValueChoice,
};
use crate::config::{ChannelConfig, HorizontalConfig, InstrumentConfig, TriggerConfig};
use crate::scpi::{ScpiError, ScpiSession};
use crate::waveform::{ChannelTrace, WaveformError};

/// Siglent SDG series AWG. Command surface is from the SDG programming guide
/// (PG02_E05C): socket port 5025, `C1`/`C2` `OUTP` / `BSWV`.
///
/// This is not an oscilloscope. Fetch plots a preview of the programmed basic
/// wave; it does not digitise the output.
pub struct Siglent {
    name: String,
    max_sine_hz: f64,
}

impl Siglent {
    pub fn from_idn(idn: &str) -> Self {
        let model = idn
            .split(',')
            .nth(1)
            .unwrap_or("")
            .trim()
            .to_ascii_uppercase();
        let max_sine_hz = if model.contains("1062") || model.contains("2042") {
            60e6
        } else if model.contains("2122") {
            120e6
        } else {
            30e6
        };
        Self {
            name: if model.is_empty() {
                "Siglent SDG".into()
            } else {
                format!("Siglent {model}")
            },
            max_sine_hz,
        }
    }
}

impl Backend for Siglent {
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
                format!("Sine max ({:.0} MHz)", self.max_sine_hz / 1e6),
                self.max_sine_hz,
            )],
            record_lengths: vec![2_000],
            trigger_modes: vec!["NONE".into()],
            trigger_slopes: vec!["RISE".into()],
            trigger_couplings: vec!["DC".into()],
            acquisition_modes: vec!["PREVIEW".into()],
            stop_after: vec!["RUNSTOP".into()],
            channel_hint: Some(
                "SDG output load is 50 Ω or HiZ. Fetch draws a preview of the programmed wave."
                    .into(),
            ),
            horizontal_hint: Some("Timebase follows two periods of the selected wave.".into()),
            acquisition_hint: Some(
                "This generator does not acquire; Fetch plots a preview.".into(),
            ),
            kind: InstrumentKind::Generator,
            channel_count: 2,
            wave_types: strings(&["SINE", "SQUARE", "RAMP", "PULSE", "NOISE", "ARB", "DC"]),
            output_pairs: Vec::new(),
        }
    }

    fn channel_enabled(&self, s: &mut ScpiSession, n: usize) -> Result<bool, ScpiError> {
        Ok(parse_outp(&s.query(&format!("C{n}:OUTP?"))?)?.on)
    }

    fn set_channel_enabled(
        &self,
        s: &mut ScpiSession,
        n: usize,
        on: bool,
    ) -> Result<(), ScpiError> {
        s.write(&format!("C{n}:OUTP {}", if on { "ON" } else { "OFF" }))
    }

    fn termination_ohms(&self, s: &mut ScpiSession, n: usize) -> Result<f64, ScpiError> {
        Ok(parse_outp(&s.query(&format!("C{n}:OUTP?"))?)?.load_ohms)
    }

    fn set_termination_ohms(
        &self,
        s: &mut ScpiSession,
        n: usize,
        ohms: f64,
    ) -> Result<(), ScpiError> {
        let load = if ohms < 1000.0 { "50" } else { "HZ" };
        s.write(&format!("C{n}:OUTP LOAD,{load}"))
    }

    fn bandwidth_hz(&self, _s: &mut ScpiSession, _n: usize) -> Result<f64, ScpiError> {
        Ok(self.max_sine_hz)
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

    fn apply_horizontal(
        &self,
        _s: &mut ScpiSession,
        _h: &crate::config::HorizontalConfig,
    ) -> Result<(), ScpiError> {
        Ok(())
    }

    fn apply_trigger(
        &self,
        _s: &mut ScpiSession,
        _t: &crate::config::TriggerConfig,
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
        apply_channel(s, n, ch)
    }

    fn fetch_channel(&self, s: &mut ScpiSession, ch: &str) -> Result<ChannelTrace, WaveformError> {
        let n = channel_number(ch)
            .ok_or_else(|| WaveformError::Parse(format!("SDG channel {ch} is not C1/C2")))?;
        if n > 2 {
            return Err(WaveformError::Parse("SDG1032X has two channels".into()));
        }
        let wave = parse_bswv(&s.query(&format!("C{n}:BSWV?"))?)?;
        Ok(preview_trace(&format!("CH{n}"), &wave))
    }

    fn wait_sequence(&self, _s: &mut ScpiSession, _timeout: Duration) -> Result<(), ScpiError> {
        Ok(())
    }

    fn acquisition_status(&self, s: &mut ScpiSession) -> Result<AcquisitionStatus, ScpiError> {
        let c1 = parse_outp(&s.query("C1:OUTP?")?)?.on;
        let c2 = parse_outp(&s.query("C2:OUTP?")?)?.on;
        Ok(AcquisitionStatus {
            running: c1 || c2,
            display: format!(
                "C1 {}  C2 {}",
                if c1 { "OUT" } else { "OFF" },
                if c2 { "OUT" } else { "OFF" }
            ),
        })
    }

    fn autoset(&self, _s: &mut ScpiSession) -> Result<(), ScpiError> {
        Err(ScpiError::Unsupported(
            "SDG has no autoset; set frequency and amplitude on the channel.".into(),
        ))
    }
}

pub fn read_config(
    session: &mut ScpiSession,
    backend: &dyn Backend,
) -> Result<InstrumentConfig, ScpiError> {
    let mut channels = [
        dummy_channel(),
        dummy_channel(),
        dummy_channel(),
        dummy_channel(),
    ];
    for n in 1..=2 {
        channels[n - 1] = read_sdg_channel(session, backend, n)?;
    }
    let freq = channels
        .iter()
        .find(|ch| ch.enabled && ch.frequency_hz > 0.0)
        .map(|ch| ch.frequency_hz)
        .unwrap_or(channels[0].frequency_hz)
        .max(1.0);
    let running = channels[0].enabled || channels[1].enabled;
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
    n: usize,
    ch: &ChannelConfig,
) -> Result<(), ScpiError> {
    if n > 2 {
        return Err(ScpiError::Unsupported("SDG1032X has two channels".into()));
    }
    let wvtp = ch.wave_type.to_ascii_uppercase();
    session.write(&format!("C{n}:BSWV WVTP,{wvtp}"))?;
    match wvtp.as_str() {
        "NOISE" => {
            session.write(&format!("C{n}:BSWV STDEV,{}", (ch.scale / 2.0).max(0.0)))?;
            session.write(&format!("C{n}:BSWV MEAN,{}", ch.offset))?;
        }
        "DC" => {
            session.write(&format!("C{n}:BSWV OFST,{}", ch.offset))?;
        }
        _ => {
            session.write(&format!("C{n}:BSWV FRQ,{}", ch.frequency_hz.max(1e-6)))?;
            session.write(&format!("C{n}:BSWV AMP,{}", ch.scale.max(0.001)))?;
            session.write(&format!("C{n}:BSWV OFST,{}", ch.offset))?;
        }
    }
    let load = if ch.termination_ohms < 1000.0 {
        "50"
    } else {
        "HZ"
    };
    session.write(&format!("C{n}:OUTP LOAD,{load}"))?;
    session.write(&format!(
        "C{n}:OUTP {}",
        if ch.enabled { "ON" } else { "OFF" }
    ))?;
    Ok(())
}

fn read_sdg_channel(
    session: &mut ScpiSession,
    backend: &dyn Backend,
    n: usize,
) -> Result<ChannelConfig, ScpiError> {
    let outp = parse_outp(&session.query(&format!("C{n}:OUTP?"))?)?;
    let wave = parse_bswv(&session.query(&format!("C{n}:BSWV?"))?)?;
    Ok(ChannelConfig {
        enabled: outp.on,
        scale: if wave.wvtp == "NOISE" {
            wave.stdev * 2.0
        } else {
            wave.amp
        },
        position: 0.0,
        offset: if wave.wvtp == "NOISE" {
            wave.mean
        } else {
            wave.ofst
        },
        coupling: "DC".into(),
        termination_ohms: outp.load_ohms,
        bandwidth_hz: backend.bandwidth_hz(session, n)?,
        probe_gain: 1.0,
        probe_type: "generator".into(),
        wave_type: wave.wvtp,
        frequency_hz: wave.frq,
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
        bandwidth_hz: 30e6,
        probe_gain: 1.0,
        probe_type: "unused".into(),
        wave_type: "SINE".into(),
        frequency_hz: 1e3,
    }
}

#[derive(Debug, Clone)]
struct Outp {
    on: bool,
    load_ohms: f64,
}

#[derive(Debug, Clone)]
struct BasicWave {
    wvtp: String,
    frq: f64,
    amp: f64,
    ofst: f64,
    duty: f64,
    sym: f64,
    phse: f64,
    stdev: f64,
    mean: f64,
}

fn parse_outp(resp: &str) -> Result<Outp, ScpiError> {
    let body = after_header(resp);
    let mut parts = body.split(',').map(str::trim);
    let state = parts
        .next()
        .ok_or_else(|| ScpiError::Parse(format!("OUTP empty: {resp:?}")))?;
    let on = matches!(state.to_ascii_uppercase().as_str(), "ON" | "1");
    let map = remaining_pairs(parts);
    let load = map.get("LOAD").map(|s| s.as_str()).unwrap_or("HZ");
    let load_ohms = if load.eq_ignore_ascii_case("HZ") || load.eq_ignore_ascii_case("HIZ") {
        1e6
    } else {
        parse_scpi_number(load).ok_or_else(|| ScpiError::Parse(format!("OUTP load {load:?}")))?
    };
    Ok(Outp { on, load_ohms })
}

fn parse_bswv(resp: &str) -> Result<BasicWave, ScpiError> {
    let map = pairs(after_header(resp));
    Ok(BasicWave {
        wvtp: map
            .get("WVTP")
            .map(|s| s.to_ascii_uppercase())
            .unwrap_or_else(|| "SINE".into()),
        frq: map
            .get("FRQ")
            .and_then(|v| parse_scpi_number(v))
            .unwrap_or(1e3),
        amp: map
            .get("AMP")
            .and_then(|v| parse_scpi_number(v))
            .unwrap_or(1.0),
        ofst: map
            .get("OFST")
            .and_then(|v| parse_scpi_number(v))
            .unwrap_or(0.0),
        duty: map
            .get("DUTY")
            .and_then(|v| parse_scpi_number(v))
            .unwrap_or(50.0),
        sym: map
            .get("SYM")
            .and_then(|v| parse_scpi_number(v))
            .unwrap_or(50.0),
        phse: map
            .get("PHSE")
            .and_then(|v| parse_scpi_number(v))
            .unwrap_or(0.0),
        stdev: map
            .get("STDEV")
            .and_then(|v| parse_scpi_number(v))
            .unwrap_or(0.1),
        mean: map
            .get("MEAN")
            .and_then(|v| parse_scpi_number(v))
            .unwrap_or(0.0),
    })
}

fn after_header(resp: &str) -> &str {
    resp.split_once(' ')
        .map(|(_, rest)| rest)
        .unwrap_or(resp)
        .trim()
}

fn pairs(body: &str) -> HashMap<String, String> {
    remaining_pairs(body.split(',').map(str::trim))
}

fn remaining_pairs<'a>(mut parts: impl Iterator<Item = &'a str>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    while let (Some(key), Some(value)) = (parts.next(), parts.next()) {
        map.insert(key.to_ascii_uppercase(), value.to_string());
    }
    map
}

fn parse_scpi_number(text: &str) -> Option<f64> {
    let text = text.trim();
    if let Ok(value) = text.parse::<f64>() {
        return Some(value);
    }
    let end = text
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || matches!(c, '.' | '+' | '-' | 'e' | 'E')))
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    text[..end].parse().ok()
}

fn preview_trace(ch: &str, wave: &BasicWave) -> ChannelTrace {
    let n = 2000usize;
    let freq = if wave.frq > 0.0 { wave.frq } else { 1e3 };
    let span = if matches!(wave.wvtp.as_str(), "NOISE" | "DC") {
        1e-3
    } else {
        2.0 / freq
    };
    let dt = span / n as f64;
    let amp = wave.amp / 2.0;
    let duty = (wave.duty / 100.0).clamp(0.01, 0.99);
    let sym = (wave.sym / 100.0).clamp(0.01, 0.99);
    let phase = wave.phse * PI / 180.0;
    let mut seed = 0xA5A5u32;
    let points = (0..n)
        .map(|i| {
            let t = i as f64 * dt;
            let v = match wave.wvtp.as_str() {
                "DC" => wave.ofst,
                "NOISE" => {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    let u = (seed >> 8) as f64 / (1u32 << 24) as f64;
                    wave.mean + wave.stdev * (2.0 * u - 1.0)
                }
                "SQUARE" => {
                    let p = (t * freq + phase / (2.0 * PI)).rem_euclid(1.0);
                    wave.ofst + if p < duty { amp } else { -amp }
                }
                "PULSE" => {
                    let p = (t * freq).rem_euclid(1.0);
                    wave.ofst + if p < duty { amp } else { -amp }
                }
                "RAMP" => {
                    let p = (t * freq + phase / (2.0 * PI)).rem_euclid(1.0);
                    let y = if p < sym {
                        -1.0 + 2.0 * p / sym
                    } else {
                        1.0 - 2.0 * (p - sym) / (1.0 - sym)
                    };
                    wave.ofst + amp * y
                }
                _ => wave.ofst + amp * (2.0 * PI * freq * t + phase).sin(),
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
    fn parses_manual_outp_example() {
        let o = parse_outp("C1:OUTP ON,LOAD,HZ,PLRT,NOR").unwrap();
        assert!(o.on);
        assert_eq!(o.load_ohms, 1e6);
        let o = parse_outp("C1:OUTP OFF,LOAD,50,PLRT,NOR").unwrap();
        assert!(!o.on);
        assert_eq!(o.load_ohms, 50.0);
    }

    #[test]
    fn parses_manual_bswv_example() {
        let w = parse_bswv("C1:BSWV WVTP,SINE,FRQ,100HZ,PERI,0.01S,AMP,2V,OFST,0V,PHSE,0").unwrap();
        assert_eq!(w.wvtp, "SINE");
        assert_eq!(w.frq, 100.0);
        assert_eq!(w.amp, 2.0);
        assert_eq!(w.ofst, 0.0);
    }

    #[test]
    fn names_sdg1032x_from_idn() {
        let s = Siglent::from_idn("Siglent Technologies,SDG1032X,SN,1.01");
        assert_eq!(s.name, "Siglent SDG1032X");
        assert_eq!(s.max_sine_hz, 30e6);
    }

    #[test]
    fn preview_has_two_thousand_points() {
        let w = BasicWave {
            wvtp: "SINE".into(),
            frq: 1e3,
            amp: 2.0,
            ofst: 0.0,
            duty: 50.0,
            sym: 50.0,
            phse: 0.0,
            stdev: 0.1,
            mean: 0.0,
        };
        let t = preview_trace("CH1", &w);
        assert_eq!(t.points.len(), 2000);
        assert!(t.points[0][1].abs() < 1e-9);
    }
}
