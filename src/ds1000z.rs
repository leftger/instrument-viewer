//! Rigol DS1000Z / MSO1000Z series.
//!
//! The DS1000Z programming guide (PGA19110-1110, May 2019) also covers the
//! MSO1000Z models: they share the same SCPI surface and add the logic
//! analyzer (`:LA:`) and digital trigger sources. Only the analog-scope half
//! is exercised here.
//!
//! Unlike the DHO900, this series has no Tek emulation: timebase and trigger
//! use the native `:TIMebase:*` / `:TRIGger:*` spellings, bandwidth limits are
//! `20M`/`OFF`, and waveform transfer is `:WAV:FORM WORD` with the same
//! ten-field `:WAV:PRE?` preamble.

use std::thread;
use std::time::{Duration, Instant};

use crate::backend::{
    channel_number, AcquisitionStatus, Backend, InstrumentCapabilities, InstrumentKind, ValueChoice,
};
use crate::config::{
    AcquisitionConfig, ChannelConfig, HorizontalConfig, InstrumentConfig, TriggerConfig,
};
use crate::profile::WaveformFormat;
use crate::scpi::{ScpiError, ScpiSession};
use crate::waveform::{ChannelTrace, WaveformError};

/// Rigol DS1000Z / MSO1000Z. Verified against the DS1000Z programming guide.
pub struct Ds1000z {
    name: String,
    /// Reported as a channel's bandwidth when its 20 MHz limit filter is off.
    full_bandwidth_hz: f64,
}

/// The only bandwidth limit this series offers besides "off".
const BANDWIDTH_LIMIT_HZ: f64 = 20e6;

/// Maximum WORD points per `:WAV:DATA?` read (programming guide, page 2-224).
const CHUNK_POINTS: usize = 125_000;

/// The single-channel memory depths of the DS1000Z. With more channels enabled
/// the valid set shrinks (half, then a quarter).
const RECORD_LENGTHS: &[u64] = &[12_000, 120_000, 1_200_000, 12_000_000, 24_000_000];

impl Ds1000z {
    pub fn from_idn(idn: &str) -> Self {
        let model = idn
            .split(',')
            .nth(1)
            .unwrap_or("")
            .trim()
            .to_ascii_uppercase();
        let full_bandwidth_hz = match () {
            _ if model.contains("DS1054") || model.contains("MSO1054") => 50e6,
            _ if model.contains("DS1074") || model.contains("MSO1074") => 70e6,
            // DS1104Z, MSO1104Z, and anything unrecognised: series maximum.
            _ => 100e6,
        };
        Self {
            name: if model.is_empty() {
                "Rigol DS1000Z".to_string()
            } else {
                format!("Rigol {model}")
            },
            full_bandwidth_hz,
        }
    }

    /// True when the scope is stopped, which is when RAW memory is readable.
    fn stopped(&self, s: &mut ScpiSession) -> Result<bool, ScpiError> {
        Ok(self.trigger_status(s)?.trim().eq_ignore_ascii_case("STOP"))
    }

    fn trigger_status(&self, s: &mut ScpiSession) -> Result<String, ScpiError> {
        Ok(s.query(":TRIG:STAT?")?.trim().to_ascii_uppercase())
    }

    /// Memory depth in points; `AUTO` falls back to the smallest depth.
    fn memory_depth(&self, s: &mut ScpiSession) -> Result<u64, ScpiError> {
        let reply = s.query(":ACQ:MDEP?")?;
        let reply = reply.trim();
        if reply.eq_ignore_ascii_case("AUTO") {
            return Ok(RECORD_LENGTHS[0]);
        }
        reply
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite() && *v >= 1.0)
            .map(|v| v.round() as u64)
            .ok_or_else(|| ScpiError::Parse(format!(":ACQ:MDEP? returned {reply:?}")))
    }
}

impl Backend for Ds1000z {
    fn name(&self) -> &str {
        &self.name
    }

    fn waveform_format(&self) -> Option<WaveformFormat> {
        Some(WaveformFormat::RIGOL)
    }

    fn capabilities(&self) -> InstrumentCapabilities {
        InstrumentCapabilities {
            channel_couplings: strings(&["DC", "AC", "GND"]),
            terminations: vec![ValueChoice::new("1 MΩ (fixed)", 1e6)],
            termination_writable: false,
            bandwidths: vec![
                ValueChoice::new("20 MHz", BANDWIDTH_LIMIT_HZ),
                ValueChoice::new(
                    format!("Full ({:.0} MHz)", self.full_bandwidth_hz / 1e6),
                    self.full_bandwidth_hz,
                ),
            ],
            record_lengths: RECORD_LENGTHS.to_vec(),
            trigger_modes: strings(&["AUTO", "NORMAL"]),
            trigger_slopes: strings(&["RISE", "FALL", "EITHER"]),
            trigger_couplings: strings(&["DC", "AC", "LFREJ", "HFREJ"]),
            acquisition_modes: strings(&["SAMPLE", "PEAKDETECT", "AVERAGE", "ULTRA"]),
            stop_after: strings(&["RUNSTOP", "SEQUENCE"]),
            channel_hint: Some(
                "DS1000Z inputs are fixed at 1 MΩ; bandwidth is Full or 20 MHz.".into(),
            ),
            horizontal_hint: Some(
                "Memory depth is 24M with one channel, 12M with two, 6M with three/four.".into(),
            ),
            acquisition_hint: Some("Sequence uses the native :SING command.".into()),
            kind: InstrumentKind::Oscilloscope,
            channel_count: 4,
            wave_types: Vec::new(),
            output_pairs: Vec::new(),
        }
    }

    fn channel_enabled(&self, s: &mut ScpiSession, n: usize) -> Result<bool, ScpiError> {
        crate::config::query_bool(s, &format!(":CHAN{n}:DISP?"))
    }

    fn set_channel_enabled(
        &self,
        s: &mut ScpiSession,
        n: usize,
        on: bool,
    ) -> Result<(), ScpiError> {
        s.write(&format!(":CHAN{n}:DISP {}", if on { "ON" } else { "OFF" }))
    }

    /// The DS1000Z has fixed 1 MΩ inputs and no impedance command at all.
    fn termination_ohms(&self, _s: &mut ScpiSession, _n: usize) -> Result<f64, ScpiError> {
        Ok(1e6)
    }

    fn set_termination_ohms(
        &self,
        _s: &mut ScpiSession,
        _n: usize,
        ohms: f64,
    ) -> Result<(), ScpiError> {
        if (ohms - 1e6).abs() < 1.0 {
            Ok(())
        } else {
            Err(ScpiError::Unsupported(format!(
                "{} has fixed 1 MΩ inputs; cannot set {ohms:.0} Ω",
                self.name
            )))
        }
    }

    fn bandwidth_hz(&self, s: &mut ScpiSession, n: usize) -> Result<f64, ScpiError> {
        let raw = s.query(&format!(":CHAN{n}:BWL?"))?;
        match raw.trim().to_ascii_uppercase().as_str() {
            "OFF" => Ok(self.full_bandwidth_hz),
            "20M" => Ok(BANDWIDTH_LIMIT_HZ),
            other => Err(ScpiError::Parse(format!(
                ":CHAN{n}:BWL? returned {other:?}"
            ))),
        }
    }

    fn set_bandwidth_hz(&self, s: &mut ScpiSession, n: usize, hz: f64) -> Result<(), ScpiError> {
        let value = if hz <= BANDWIDTH_LIMIT_HZ {
            "20M"
        } else {
            "OFF"
        };
        s.write(&format!(":CHAN{n}:BWL {value}"))
    }

    /// No probe-ID query exists; `:CHAN<n>:PROB?` gives attenuation, not a type.
    fn probe_type(&self, _s: &mut ScpiSession, _n: usize) -> Result<String, ScpiError> {
        Ok("unknown".to_string())
    }

    /// Level is a single edge-trigger value, independent of the source.
    fn trigger_level(&self, s: &mut ScpiSession, _source: &str) -> Result<f64, ScpiError> {
        crate::config::query_f64(s, ":TRIG:EDGE:LEV?")
    }

    fn set_trigger_level(
        &self,
        s: &mut ScpiSession,
        _source: &str,
        volts: f64,
    ) -> Result<(), ScpiError> {
        s.write(&format!(":TRIG:EDGE:LEV {volts}"))
    }

    fn read_config(&self, s: &mut ScpiSession) -> Result<InstrumentConfig, ScpiError> {
        let nch = self.capabilities().channel_count;
        let mut channels = Vec::with_capacity(nch);
        for n in 1..=nch {
            channels.push(ChannelConfig {
                enabled: self.channel_enabled(s, n)?,
                scale: crate::config::query_f64(s, &format!(":CHAN{n}:SCAL?"))?,
                // No per-channel vertical position command exists.
                position: 0.0,
                offset: crate::config::query_f64(s, &format!(":CHAN{n}:OFFS?"))?,
                coupling: crate::scpi::parse_character(&s.query(&format!(":CHAN{n}:COUP?"))?),
                termination_ohms: 1e6,
                bandwidth_hz: self.bandwidth_hz(s, n)?,
                probe_gain: crate::config::query_f64(s, &format!(":CHAN{n}:PROB?"))?,
                probe_type: "unknown".into(),
                wave_type: String::new(),
                frequency_hz: 0.0,
            });
        }

        let source = source_to_read(&s.query(":TRIG:EDGE:SOUR?")?);
        let sweep = crate::scpi::parse_character(&s.query(":TRIG:SWE?")?);
        let mode = match sweep.as_str() {
            "SING" => "NORMAL".to_string(),
            other => other.to_string(),
        };
        let trigger = TriggerConfig {
            mode,
            source,
            slope: slope_to_read(&s.query(":TRIG:EDGE:SLOP?")?),
            coupling: coupling_to_read(&s.query(":TRIG:COUP?")?),
            level: self.trigger_level(s, "CH1")?,
        };

        let acquisition = AcquisitionConfig {
            mode: acquire_to_read(&s.query(":ACQ:TYPE?")?),
            stop_after: if sweep == "SING" {
                "SEQUENCE".into()
            } else {
                "RUNSTOP".into()
            },
            running: !self.stopped(s)?,
        };

        Ok(InstrumentConfig {
            channels,
            horizontal: HorizontalConfig {
                scale: crate::config::query_f64(s, ":TIM:SCAL?")?,
                // Timebase offset is in seconds on this series; our position is
                // percent, so report the mid-screen default.
                position: 50.0,
                record_length: self.memory_depth(s)?,
            },
            trigger,
            acquisition,
            output_pair: String::new(),
        })
    }

    fn apply_channel(
        &self,
        s: &mut ScpiSession,
        n: usize,
        ch: &ChannelConfig,
    ) -> Result<(), ScpiError> {
        // Probe ratio changes the engineering units of scale/offset, so set it
        // first, then the scale that depends on it.
        s.write(&format!(":CHAN{n}:PROB {}", ch.probe_gain))?;
        self.set_channel_enabled(s, n, ch.enabled)?;
        s.write(&format!(":CHAN{n}:COUP {}", ch.coupling))?;
        self.set_termination_ohms(s, n, ch.termination_ohms)?;
        self.set_bandwidth_hz(s, n, ch.bandwidth_hz)?;
        s.write(&format!(":CHAN{n}:SCAL {}", ch.scale))?;
        s.write(&format!(":CHAN{n}:OFFS {}", ch.offset))
    }

    fn apply_horizontal(&self, s: &mut ScpiSession, h: &HorizontalConfig) -> Result<(), ScpiError> {
        // The depth must be valid for the number of enabled channels; the scope
        // coerces or rejects invalid picks, and the hint explains the limits.
        s.write(&format!(":ACQ:MDEP {}", h.record_length))?;
        s.write(&format!(":TIM:SCAL {}", h.scale))
    }

    fn apply_trigger(&self, s: &mut ScpiSession, t: &TriggerConfig) -> Result<(), ScpiError> {
        s.write(":TRIG:MODE EDGE")?;
        s.write(&format!(":TRIG:SWE {}", t.mode))?;
        s.write(&format!(":TRIG:EDGE:SOUR {}", source_to_write(&t.source)))?;
        s.write(&format!(":TRIG:EDGE:SLOP {}", slope_to_write(&t.slope)))?;
        s.write(&format!(":TRIG:COUP {}", coupling_to_write(&t.coupling)))?;
        self.set_trigger_level(s, &t.source, t.level)
    }

    fn apply_acquisition(
        &self,
        s: &mut ScpiSession,
        mode: &str,
        stop_after: &str,
        running: bool,
    ) -> Result<(), ScpiError> {
        s.write(&format!(":ACQ:TYPE {}", acquire_to_write(mode)))?;
        if !running {
            s.write(":STOP")
        } else if stop_after.eq_ignore_ascii_case("SEQUENCE") {
            s.write(":SING")
        } else {
            s.write(":RUN")
        }
    }

    fn fetch_channel(&self, s: &mut ScpiSession, ch: &str) -> Result<ChannelTrace, WaveformError> {
        let n = channel_number(ch)
            .ok_or_else(|| WaveformError::Parse(format!("{} cannot source {ch}", self.name)))?;

        s.write(&format!(":WAV:SOUR CHAN{n}"))?;
        s.write(":WAV:FORM WORD")?;
        // NORM yields only the on-screen points. RAW yields the full memory,
        // but only while the scope is stopped.
        let raw_mode = self.stopped(s)?;
        s.write(if raw_mode {
            ":WAV:MODE RAW"
        } else {
            ":WAV:MODE NORM"
        })?;

        let pre = Preamble::query(s)?;
        let raw = read_points(s, pre.points, raw_mode)?;

        let points = crate::waveform::decode_samples(
            &WaveformFormat::RIGOL,
            &raw,
            |i| pre.time(i),
            |code| pre.volts(code),
        )?;

        Ok(ChannelTrace {
            channel: format!("CH{n}"),
            x_unit: "s".into(),
            y_unit: "V".into(),
            points,
        })
    }

    /// `:SING` arms one acquisition; `:TRIG:STAT?` reads `STOP` once it lands.
    fn wait_sequence(&self, s: &mut ScpiSession, timeout: Duration) -> Result<(), ScpiError> {
        s.write(":SING")?;
        let start = Instant::now();
        loop {
            thread::sleep(Duration::from_millis(100));
            if self.stopped(s)? {
                return Ok(());
            }
            if start.elapsed() > timeout {
                let _ = s.write(":STOP");
                return Err(ScpiError::Timeout);
            }
        }
    }

    fn acquisition_status(&self, s: &mut ScpiSession) -> Result<AcquisitionStatus, ScpiError> {
        let status = self.trigger_status(s)?;
        Ok(AcquisitionStatus {
            running: !status.eq_ignore_ascii_case("STOP"),
            display: status,
        })
    }

    fn autoset(&self, s: &mut ScpiSession) -> Result<(), ScpiError> {
        s.write(":AUT")
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

/// `CH1` / `1` / `CHAN1` to the DS1000Z's `CHAN1`-style trigger source.
fn source_to_write(source: &str) -> String {
    match channel_number(source) {
        Some(n) => format!("CHAN{n}"),
        None => source.to_ascii_uppercase(),
    }
}

/// The DS1000Z reports `CHAN1` where the rest of the app uses `CH1`.
fn source_to_read(reply: &str) -> String {
    let upper = reply.trim().to_ascii_uppercase();
    match channel_number(&upper) {
        Some(n) => format!("CH{n}"),
        None => upper,
    }
}

fn slope_to_write(slope: &str) -> &str {
    match slope {
        "FALL" => "NEG",
        "EITHER" => "RFAL",
        _ => "POS",
    }
}

fn slope_to_read(reply: &str) -> String {
    match reply.trim().to_ascii_uppercase().as_str() {
        "NEG" => "FALL".into(),
        "RFAL" => "EITHER".into(),
        _ => "RISE".into(),
    }
}

fn coupling_to_write(coupling: &str) -> &str {
    match coupling {
        "LFREJ" => "LFR",
        "HFREJ" => "HFR",
        other => other,
    }
}

fn coupling_to_read(reply: &str) -> String {
    match reply.trim().to_ascii_uppercase().as_str() {
        "LFR" => "LFREJ".into(),
        "HFR" => "HFREJ".into(),
        other => other.to_string(),
    }
}

fn acquire_to_write(mode: &str) -> &str {
    match mode {
        "SAMPLE" => "NORM",
        "PEAKDETECT" => "PEAK",
        "AVERAGE" => "AVER",
        // The DS1000Z calls high-resolution "HRES" where newer scopes say ULTRA.
        "ULTRA" | "HIRES" => "HRES",
        other => other,
    }
}

fn acquire_to_read(reply: &str) -> String {
    match reply.trim().to_ascii_uppercase().as_str() {
        "NORM" => "SAMPLE".into(),
        "PEAK" => "PEAKDETECT".into(),
        "AVER" => "AVERAGE".into(),
        "HRES" => "ULTRA".into(),
        other => other.to_string(),
    }
}

/// The ten comma-separated fields of `:WAV:PRE?` (DS1000Z programming guide,
/// page 2-230): format, type, points, count, xincrement, xorigin, xreference,
/// yincrement, yorigin, yreference.
#[derive(Debug, Clone, PartialEq)]
struct Preamble {
    points: usize,
    xincrement: f64,
    xorigin: f64,
    xreference: f64,
    yincrement: f64,
    yorigin: f64,
    yreference: f64,
}

impl Preamble {
    fn query(s: &mut ScpiSession) -> Result<Self, WaveformError> {
        Self::parse(&s.query(":WAV:PRE?")?)
    }

    fn parse(resp: &str) -> Result<Self, WaveformError> {
        let f: Vec<&str> = resp.split(',').map(str::trim).collect();
        if f.len() < 10 {
            return Err(WaveformError::Parse(format!(
                ":WAV:PRE? has {} fields, expected 10: {resp}",
                f.len()
            )));
        }
        let num = |i: usize, name: &str| -> Result<f64, WaveformError> {
            f[i].parse::<f64>()
                .map_err(|_| WaveformError::Parse(format!("{name} = {:?}", f[i])))
        };
        let points = num(2, "points")?;
        if !points.is_finite() || points < 1.0 {
            return Err(WaveformError::Parse(format!(
                ":WAV:PRE? reports {points} points"
            )));
        }
        Ok(Self {
            points: points as usize,
            xincrement: num(4, "xincrement")?,
            xorigin: num(5, "xorigin")?,
            xreference: num(6, "xreference")?,
            yincrement: num(7, "yincrement")?,
            yorigin: num(8, "yorigin")?,
            yreference: num(9, "yreference")?,
        })
    }

    fn time(&self, i: usize) -> f64 {
        self.xorigin + self.xincrement * (i as f64 - self.xreference)
    }

    fn volts(&self, level: f64) -> f64 {
        (level - self.yorigin - self.yreference) * self.yincrement
    }
}

/// Read `points` WORD samples, windowing long RAW records through
/// `:WAV:STAR` / `:WAV:STOP` (max 125 000 points per `:WAV:DATA?`).
fn read_points(
    s: &mut ScpiSession,
    points: usize,
    raw_mode: bool,
) -> Result<Vec<u8>, WaveformError> {
    let expect = |block: &[u8], want: usize| -> Result<(), WaveformError> {
        if block.len() == want {
            Ok(())
        } else {
            Err(WaveformError::Parse(format!(
                ":WAV:DATA? returned {} bytes, expected {want}",
                block.len()
            )))
        }
    };

    if !raw_mode {
        let block = s.query_binary_block(":WAV:DATA?")?;
        expect(&block, points * 2)?;
        return Ok(block);
    }

    let mut out = Vec::with_capacity(points * 2);
    let mut start = 1usize;
    while start <= points {
        let end = (start + CHUNK_POINTS - 1).min(points);
        s.write(&format!(":WAV:STAR {start}"))?;
        s.write(&format!(":WAV:STOP {end}"))?;
        let _ = s.query("*OPC?")?;
        let block = s.query_binary_block(":WAV:DATA?")?;
        expect(&block, (end - start + 1) * 2)?;
        out.extend_from_slice(&block);
        start = end + 1;
        if start <= points {
            // Give the scope a moment before the next window request.
            thread::sleep(Duration::from_millis(20));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_name_and_bandwidth_from_idn() {
        let mso = Ds1000z::from_idn("RIGOL TECHNOLOGIES,MSO1104Z,DS1ZD123456789,00.04.03.SP2");
        assert_eq!(mso.name(), "Rigol MSO1104Z");
        assert_eq!(mso.full_bandwidth_hz, 100e6);
        assert_eq!(
            Ds1000z::from_idn("RIGOL,DS1054Z,X,1").full_bandwidth_hz,
            50e6
        );
        assert_eq!(
            Ds1000z::from_idn("RIGOL,DS1074Z,X,1").full_bandwidth_hz,
            70e6
        );
    }

    #[test]
    fn maps_trigger_source_between_ch1_and_chan1() {
        assert_eq!(source_to_write("CH1"), "CHAN1");
        assert_eq!(source_to_write("CHAN4"), "CHAN4");
        assert_eq!(source_to_write("D7"), "D7");
        assert_eq!(source_to_read("CHAN2"), "CH2");
        assert_eq!(source_to_read("D15"), "D15");
    }

    #[test]
    fn maps_slopes_and_couplings() {
        assert_eq!(slope_to_write("RISE"), "POS");
        assert_eq!(slope_to_write("FALL"), "NEG");
        assert_eq!(slope_to_write("EITHER"), "RFAL");
        assert_eq!(slope_to_read("NEG"), "FALL");
        assert_eq!(slope_to_read("RFAL"), "EITHER");
        assert_eq!(slope_to_read("POS"), "RISE");
        assert_eq!(coupling_to_write("LFREJ"), "LFR");
        assert_eq!(coupling_to_write("HFREJ"), "HFR");
        assert_eq!(coupling_to_read("LFR"), "LFREJ");
        assert_eq!(coupling_to_read("HFR"), "HFREJ");
    }

    #[test]
    fn maps_acquisition_modes() {
        assert_eq!(acquire_to_write("SAMPLE"), "NORM");
        assert_eq!(acquire_to_write("PEAKDETECT"), "PEAK");
        assert_eq!(acquire_to_write("AVERAGE"), "AVER");
        assert_eq!(acquire_to_write("ULTRA"), "HRES");
        assert_eq!(acquire_to_read("NORM"), "SAMPLE");
        assert_eq!(acquire_to_read("HRES"), "ULTRA");
    }

    #[test]
    fn parses_ds1000z_preamble() {
        let p =
            Preamble::parse("1,2,12000,1,4.000000e-09,-2.400000e-05,0,4.000000e-02,0,127").unwrap();
        assert_eq!(p.points, 12_000);
        assert_eq!(p.xincrement, 4e-9);
        assert_eq!(p.xorigin, -2.4e-5);
        assert_eq!(p.yincrement, 0.04);
        assert_eq!(p.yreference, 127.0);
        // First sample sits at xorigin; the reference code reads 0 V.
        assert_eq!(p.time(0), -2.4e-5);
        assert!(p.volts(127.0).abs() < 1e-12);
        assert!((p.volts(128.0) - 0.04).abs() < 1e-12);
    }

    #[test]
    fn rejects_short_preamble() {
        let err = Preamble::parse("1,2,12000").unwrap_err();
        assert!(err.to_string().contains("expected 10"), "{err}");
    }
}
