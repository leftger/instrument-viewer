use std::thread;
use std::time::{Duration, Instant};

use crate::backend::{
    channel_number, AcquisitionStatus, Backend, InstrumentCapabilities, ValueChoice,
};
use crate::scpi::{ScpiError, ScpiSession};
use crate::waveform::{ChannelTrace, WaveformError};

/// Rigol DHO900. Verified against a DHO924S running firmware 00.01.05.
///
/// The firmware carries a partial Tektronix emulation, so most of `config.rs`
/// works untouched. What it does *not* emulate is the entire waveform transfer
/// (`DATA:*`, `WFMOutpre?`, `CURVE?` are all rejected) and channel enable.
///
/// One command is worse than unsupported: `ACQUIRE:STOPAFTER SEQUENCE` is
/// accepted and reads back as `SEQuence`, but has no effect — the scope runs on
/// forever. Single-shot has to go through `:SING`.
pub struct Rigol {
    name: String,
    /// Reported as a channel's bandwidth when its limit filter is off.
    full_bandwidth_hz: f64,
}

/// Largest point count to pull in one `:WAV:DATA?`. Long RAW records have to be
/// read in windows set by `:WAV:STAR` / `:WAV:STOP`.
const CHUNK_POINTS: usize = 100_000;

/// The only bandwidth limit the DHO900 offers besides "off".
const BANDWIDTH_LIMIT_HZ: f64 = 20e6;

impl Rigol {
    pub fn from_idn(idn: &str) -> Self {
        let model = idn
            .split(',')
            .nth(1)
            .unwrap_or("")
            .trim()
            .to_ascii_uppercase();
        let full_bandwidth_hz = match () {
            // DHO914/DHO914S are 125 MHz parts, DHO924/DHO924S are 250 MHz.
            // Anything unrecognised falls back to the series maximum; this value
            // is only ever reported, never used to configure the instrument.
            _ if model.starts_with("DHO91") => 125e6,
            _ => 250e6,
        };
        Self {
            name: if model.is_empty() {
                "Rigol".to_string()
            } else {
                format!("Rigol {model}")
            },
            full_bandwidth_hz,
        }
    }

    /// True when the scope is not acquiring, which is when RAW is readable.
    fn stopped(&self, s: &mut ScpiSession) -> Result<bool, ScpiError> {
        let state = s.query("ACQUIRE:STATE?")?;
        Ok(!matches!(
            state.trim().to_ascii_uppercase().as_str(),
            "1" | "ON" | "RUN"
        ))
    }
}

impl Backend for Rigol {
    fn name(&self) -> &str {
        &self.name
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
            record_lengths: vec![
                1_000, 10_000, 100_000, 1_000_000, 10_000_000, 25_000_000, 50_000_000,
            ],
            trigger_modes: strings(&["AUTO", "NORMAL"]),
            trigger_slopes: strings(&["RISE", "FALL", "EITHER"]),
            trigger_couplings: strings(&["DC", "AC", "LFREJ", "HFREJ"]),
            acquisition_modes: strings(&["SAMPLE", "PEAKDETECT", "AVERAGE", "ULTRA"]),
            stop_after: strings(&["RUNSTOP", "SEQUENCE"]),
            channel_hint: Some(
                "DHO900 inputs are fixed at 1 MΩ; bandwidth is Full or 20 MHz.".into(),
            ),
            horizontal_hint: Some(
                "Maximum memory is 50M with one channel, 25M with two, and 10M with all four."
                    .into(),
            ),
            acquisition_hint: Some(
                "Sequence uses the native :SING command; STOPAFTER alone is ineffective.".into(),
            ),
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

    /// `CH<n>:TERMINATION?` answers with the enum `MEG`, not an ohm count.
    fn termination_ohms(&self, s: &mut ScpiSession, n: usize) -> Result<f64, ScpiError> {
        let raw = s.query(&format!(":CHAN{n}:IMP?"))?;
        match raw.trim().to_ascii_uppercase().as_str() {
            "OMEG" | "MEG" => Ok(1e6),
            "FIFT" | "FIFTY" => Ok(50.0),
            _ => Err(ScpiError::Parse(format!(":CHAN{n}:IMP? returned {raw:?}"))),
        }
    }

    /// The DHO900 has fixed 1 MΩ inputs: `:CHAN<n>:IMP` rejects even the value it
    /// currently reports. Writing back an unchanged value has to stay silent so
    /// that editing an unrelated field on the same channel still applies.
    fn set_termination_ohms(
        &self,
        s: &mut ScpiSession,
        n: usize,
        ohms: f64,
    ) -> Result<(), ScpiError> {
        let current = self.termination_ohms(s, n)?;
        if (current - ohms).abs() < 1.0 {
            return Ok(());
        }
        Err(ScpiError::Unsupported(format!(
            "{} has fixed {current:.0} Ω inputs; cannot set {ohms:.0} Ω",
            self.name
        )))
    }

    fn bandwidth_hz(&self, s: &mut ScpiSession, n: usize) -> Result<f64, ScpiError> {
        let raw = s.query(&format!(":CHAN{n}:BWL?"))?;
        match raw.trim().to_ascii_uppercase().as_str() {
            "OFF" => Ok(self.full_bandwidth_hz),
            "20M" => Ok(BANDWIDTH_LIMIT_HZ),
            other => other
                .strip_suffix('M')
                .and_then(|mhz| mhz.parse::<f64>().ok())
                .map(|mhz| mhz * 1e6)
                .ok_or_else(|| ScpiError::Parse(format!(":CHAN{n}:BWL? returned {raw:?}"))),
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

    /// No equivalent exists: `:CHAN<n>:PROB?` gives attenuation, not a probe type.
    fn probe_type(&self, _s: &mut ScpiSession, _n: usize) -> Result<String, ScpiError> {
        Ok("unknown".to_string())
    }

    /// The suffixed `TRIGGER:A:LEVEL:<source>` form is rejected; the bare one works.
    fn trigger_level(&self, s: &mut ScpiSession, _source: &str) -> Result<f64, ScpiError> {
        crate::config::query_f64(s, "TRIGGER:A:LEVEL?")
    }

    fn set_trigger_level(
        &self,
        s: &mut ScpiSession,
        _source: &str,
        volts: f64,
    ) -> Result<(), ScpiError> {
        s.write(&format!("TRIGGER:A:LEVEL {volts}"))
    }

    fn apply_acquisition(
        &self,
        s: &mut ScpiSession,
        mode: &str,
        stop_after: &str,
        running: bool,
    ) -> Result<(), ScpiError> {
        s.write(&format!("ACQUIRE:MODE {mode}"))?;
        // Keep the emulated setting coherent for readback, but use the native
        // action commands: the Rigol accepts STOPAFTER SEQUENCE without making
        // the acquisition stop.
        s.write(&format!("ACQUIRE:STOPAFTER {stop_after}"))?;
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
        // NORM yields only the 1000 on-screen points. RAW yields the full
        // acquisition memory, but only while the scope is stopped.
        let raw_mode = self.stopped(s)?;
        s.write(if raw_mode {
            ":WAV:MODE RAW"
        } else {
            ":WAV:MODE NORM"
        })?;

        // After a chunked read, WAV:PRE? keeps reporting the final transfer
        // window (for example 100k) rather than total acquisition memory.
        // ACQ:MDEP? remains the authoritative RAW point count.
        let raw_points = if raw_mode {
            Some(query_point_count(s, ":ACQ:MDEP?")?)
        } else {
            None
        };
        let pre = Preamble::query(s)?;
        let raw = read_points(s, raw_points.unwrap_or(pre.points), raw_mode)?;

        let points = raw
            .as_chunks::<2>()
            .0
            .iter()
            .enumerate()
            .map(|(i, c)| {
                // WORD samples are unsigned and little-endian; there is no
                // :WAV:BYTeorder on this firmware to change that.
                let level = u16::from_le_bytes([c[0], c[1]]) as f64;
                [pre.time(i), pre.volts(level)]
            })
            .collect();

        Ok(ChannelTrace {
            channel: format!("CH{n}"),
            // The preamble carries no unit strings; these axes are always s and V.
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
            if s.query(":TRIG:STAT?")?.trim().eq_ignore_ascii_case("STOP") {
                return Ok(());
            }
            if start.elapsed() > timeout {
                let _ = s.write(":STOP");
                return Err(ScpiError::Timeout);
            }
        }
    }

    fn acquisition_status(&self, s: &mut ScpiSession) -> Result<AcquisitionStatus, ScpiError> {
        let running = crate::config::query_bool(s, "ACQUIRE:STATE?")?;
        let trigger = normalize_trigger_status(&s.query(":TRIG:STAT?")?);
        Ok(AcquisitionStatus {
            running,
            display: trigger,
        })
    }

    fn autoset(&self, s: &mut ScpiSession) -> Result<(), ScpiError> {
        s.write(":AUT")
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn query_point_count(s: &mut ScpiSession, command: &str) -> Result<usize, WaveformError> {
    let response = s.query(command)?;
    parse_point_count(command, &response)
}

fn parse_point_count(command: &str, response: &str) -> Result<usize, WaveformError> {
    let points = response
        .trim()
        .parse::<f64>()
        .map_err(|_| WaveformError::Parse(format!("{command} returned {response:?}")))?;
    if !points.is_finite() || points < 1.0 || points > usize::MAX as f64 {
        return Err(WaveformError::Parse(format!(
            "{command} returned invalid point count {response:?}"
        )));
    }
    Ok(points.round() as usize)
}

fn normalize_trigger_status(value: &str) -> String {
    match value.trim().to_ascii_uppercase().as_str() {
        "TRIGGERED" => "TD".into(),
        "WAITING" => "WAIT".into(),
        other => other.to_string(),
    }
}

/// The ten comma-separated fields of `:WAV:PRE?`.
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

/// Read `points` WORD samples, windowing long RAW records.
///
/// `:WAV:STAR` / `:WAV:STOP` are only meaningful in RAW mode, so a screen-mode
/// read is left as a single plain `:WAV:DATA?`.
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

    // RAW honors the current transfer window, which may have been left at a
    // smaller range by a prior read. Set it explicitly even for one chunk.
    let mut out = Vec::with_capacity(points * 2);
    let mut start = 1usize;
    while start <= points {
        let end = (start + CHUNK_POINTS - 1).min(points);
        s.write(&format!(":WAV:STAR {start}"))?;
        s.write(&format!(":WAV:STOP {end}"))?;
        // Without a round trip here the DHO900 can receive DATA? before it has
        // applied the new window and then never answer. Tracing happened to
        // mask this race by slowing the command stream.
        let _ = s.query("*OPC?")?;
        let block = s.query_binary_block(":WAV:DATA?")?;
        expect(&block, (end - start + 1) * 2)?;
        out.extend_from_slice(&block);
        start = end + 1;
        if start <= points {
            // The socket returns before the scope is ready to accept the next
            // window. A short inter-block pause prevents the following STAR
            // command from being lost on large records.
            thread::sleep(Duration::from_millis(20));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured verbatim from a RIGOL DHO924S, firmware 00.01.05.
    const REAL: &str = "1,0,1000,1,2.000000E-6,-1.000000E-3,0.000000,6.6667E-06,-22650,32768";

    #[test]
    fn parses_real_preamble() {
        let p = Preamble::parse(REAL).unwrap();
        assert_eq!(p.points, 1000);
        assert_eq!(p.xincrement, 2e-6);
        assert_eq!(p.xorigin, -1e-3);
        assert_eq!(p.xreference, 0.0);
        assert_eq!(p.yincrement, 6.6667e-6);
        assert_eq!(p.yorigin, -22650.0);
        assert_eq!(p.yreference, 32768.0);
    }

    #[test]
    fn scales_from_preamble() {
        let p = Preamble::parse(REAL).unwrap();
        // First sample sits at xorigin, not at zero.
        assert_eq!(p.time(0), -1e-3);
        assert!((p.time(500) - 0.0).abs() < 1e-12);
        // A code equal to yorigin+yreference is 0 V by construction.
        assert!(p.volts(-22650.0 + 32768.0).abs() < 1e-12);
        // One code above that is one yincrement.
        assert!((p.volts(-22650.0 + 32768.0 + 1.0) - 6.6667e-6).abs() < 1e-12);
    }

    #[test]
    fn applies_nonzero_xreference() {
        let p = Preamble::parse("1,0,1000,1,2.0E-6,-1.0E-3,500,6.6E-06,-22650,32768").unwrap();
        assert!((p.time(500) + 1e-3).abs() < 1e-12);
    }

    #[test]
    fn rejects_short_preamble() {
        let err = Preamble::parse("1,0,1000,1").unwrap_err();
        assert!(err.to_string().contains("expected 10"), "{err}");
    }

    #[test]
    fn rejects_zero_point_preamble() {
        let s = "1,0,0,1,2.0E-6,-1.0E-3,0.0,6.6E-06,-22650,32768";
        assert!(Preamble::parse(s).is_err());
    }

    /// WORD samples are unsigned little-endian, so the low byte comes first.
    #[test]
    fn decodes_little_endian_unsigned_words() {
        assert_eq!(u16::from_le_bytes([0x10, 0x80]), 0x8010);
        // Big-endian decoding of the same pair would land far away.
        assert_ne!(u16::from_le_bytes([0x10, 0x80]), 0x1080);
    }

    #[test]
    fn derives_bandwidth_and_name_from_idn() {
        let r = Rigol::from_idn("RIGOL TECHNOLOGIES,DHO924S,SN,00.01.05");
        assert_eq!(r.name(), "Rigol DHO924S");
        assert_eq!(r.full_bandwidth_hz, 250e6);
        assert_eq!(
            Rigol::from_idn("RIGOL,DHO914S,X,1").full_bandwidth_hz,
            125e6
        );
    }

    #[test]
    fn normalizes_trigger_status_for_display() {
        assert_eq!(normalize_trigger_status("TD\n"), "TD");
        assert_eq!(normalize_trigger_status("waiting"), "WAIT");
        assert_eq!(normalize_trigger_status("Triggered"), "TD");
        assert_eq!(normalize_trigger_status("STOP"), "STOP");
    }

    #[test]
    fn parses_raw_memory_depth_in_scientific_notation() {
        assert_eq!(
            parse_point_count(":ACQ:MDEP?", "1.0000E+06").unwrap(),
            1_000_000
        );
        assert!(parse_point_count(":ACQ:MDEP?", "0").is_err());
        assert!(parse_point_count(":ACQ:MDEP?", "AUTO").is_err());
    }
}
