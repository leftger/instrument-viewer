//! Hantek HRDO2000 series digital oscilloscope.
//!
//! The command set is Rigol-DS1000Z-like (`:CHANnel<n>:*`, `:TIMebase:*`,
//! `:TRIGger:*`, `:ACQuire:*`), with two Hantek twists:
//!
//! * run/stop is `:RUNing ON|OFF` (there is no `:STOP`), and single-shot is
//!   `:SINGle`;
//! * waveform transfer carries no `:WAVeform:PREamble?`. Instead
//!   `:WAVeform:DATA:DISP? Channel<n>` answers with a fixed 128-byte binary
//!   header (offsets, vertical scales, sample rate, pre-trigger time) followed
//!   by one byte per sample, scaled as
//!   `(scale * probe / 24) * (code - 128) - offset * probe`.

use std::thread;
use std::time::{Duration, Instant};

use crate::backend::{
    channel_number, AcquisitionStatus, Backend, InstrumentCapabilities, InstrumentKind, ValueChoice,
};
use crate::config::{
    AcquisitionConfig, ChannelConfig, HorizontalConfig, InstrumentConfig, TriggerConfig,
};
use crate::scpi::{ScpiError, ScpiSession};
use crate::waveform::{ChannelTrace, WaveformError};

/// Bytes of binary header before the sample payload.
const HEADER_BYTES: usize = 128;
/// Offset of the ASCII payload byte count inside the header.
const HEADER_LEN_OFFSET: usize = 11;
/// Width of that ASCII byte count.
const HEADER_LEN_DIGITS: usize = 9;
/// The only bandwidth limit besides "off".
const BANDWIDTH_LIMITS_HZ: &[f64] = &[20e6, 100e6, 200e6, 350e6];
const RECORD_LENGTHS: &[u64] = &[1_000, 10_000, 100_000, 1_000_000, 10_000_000, 25_000_000];

/// Hantek HRDO2000 series.
pub struct Hrdo2000 {
    name: String,
    full_bandwidth_hz: f64,
}

impl Hrdo2000 {
    pub fn from_idn(idn: &str) -> Self {
        let model = idn
            .split(',')
            .nth(1)
            .unwrap_or("")
            .trim()
            .to_ascii_uppercase();
        // The guide does not list per-model bandwidths; model numbers that
        // embed 100 or 350 set the reported full bandwidth, otherwise the
        // series default of 200 MHz. This value is only ever reported.
        let full_bandwidth_hz = if model.contains("350") {
            350e6
        } else if model.contains("100") {
            100e6
        } else {
            200e6
        };
        Self {
            name: if model.is_empty() {
                "Hantek HRDO2000".to_string()
            } else {
                format!("Hantek {model}")
            },
            full_bandwidth_hz,
        }
    }

    fn trigger_status(&self, s: &mut ScpiSession) -> Result<String, ScpiError> {
        Ok(s.query(":TRIG:STAT?")?.trim().to_ascii_uppercase())
    }

    fn stopped(&self, s: &mut ScpiSession) -> Result<bool, ScpiError> {
        Ok(self.trigger_status(s)?.eq_ignore_ascii_case("STOP"))
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

    fn probe_ratio(&self, s: &mut ScpiSession, n: usize) -> f64 {
        crate::config::query_f64(s, &format!(":CHAN{n}:PROB?"))
            .ok()
            .filter(|ratio| ratio.is_finite() && *ratio > 0.0)
            .unwrap_or(1.0)
    }
}

impl Backend for Hrdo2000 {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> InstrumentCapabilities {
        let mut bandwidths: Vec<ValueChoice> = BANDWIDTH_LIMITS_HZ
            .iter()
            .map(|hz| ValueChoice::new(format!("{:.0} MHz", hz / 1e6), *hz))
            .collect();
        if !bandwidths
            .iter()
            .any(|choice| (choice.value - self.full_bandwidth_hz).abs() < 1.0)
        {
            bandwidths.push(ValueChoice::new(
                format!("Full ({:.0} MHz)", self.full_bandwidth_hz / 1e6),
                self.full_bandwidth_hz,
            ));
        }
        InstrumentCapabilities {
            channel_couplings: strings(&["DC", "AC", "GND"]),
            terminations: vec![ValueChoice::new("1 MΩ (fixed)", 1e6)],
            termination_writable: false,
            bandwidths,
            record_lengths: RECORD_LENGTHS.to_vec(),
            trigger_modes: strings(&["AUTO", "NORMAL"]),
            trigger_slopes: strings(&["RISE", "FALL", "EITHER"]),
            // The series has no `:TRIGger:COUPling` command in its guide.
            trigger_couplings: strings(&["DC"]),
            acquisition_modes: strings(&["SAMPLE", "PEAKDETECT", "AVERAGE", "ULTRA"]),
            stop_after: strings(&["RUNSTOP", "SEQUENCE"]),
            channel_hint: Some(
                "HRDO2000 inputs are fixed at 1 MΩ; bandwidth is 20/100/200/350 MHz or off.".into(),
            ),
            horizontal_hint: Some(
                "Memory depth is 1k to 25M points; the header carries the sample rate.".into(),
            ),
            acquisition_hint: Some("Sequence uses :SINGle; run/stop uses :RUNing ON|OFF.".into()),
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
            other => other
                .strip_suffix('M')
                .and_then(|mhz| mhz.parse::<f64>().ok())
                .map(|mhz| mhz * 1e6)
                .ok_or_else(|| ScpiError::Parse(format!(":CHAN{n}:BWL? returned {other:?}"))),
        }
    }

    fn set_bandwidth_hz(&self, s: &mut ScpiSession, n: usize, hz: f64) -> Result<(), ScpiError> {
        let value = if hz >= self.full_bandwidth_hz {
            "OFF".to_string()
        } else {
            format!("{:.0}M", hz / 1e6)
        };
        s.write(&format!(":CHAN{n}:BWL {value}"))
    }

    fn probe_type(&self, _s: &mut ScpiSession, _n: usize) -> Result<String, ScpiError> {
        Ok("unknown".to_string())
    }

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
                probe_gain: self.probe_ratio(s, n),
                probe_type: "unknown".into(),
                wave_type: String::new(),
                frequency_hz: 0.0,
            });
        }

        let sweep = crate::scpi::parse_character(&s.query(":TRIG:SWE?")?);
        let source = source_to_read(&s.query(":TRIG:EDGE:SOUR?")?);
        let trigger = TriggerConfig {
            mode: if sweep == "SING" {
                "NORMAL".into()
            } else {
                sweep.clone()
            },
            source,
            slope: slope_to_read(&s.query(":TRIG:EDGE:SLOP?")?),
            coupling: "DC".into(),
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
                scale: crate::config::query_f64(s, ":TIM:MAIN:SCAL?")?,
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
        // The probe ratio changes the engineering units of scale/offset.
        s.write(&format!(":CHAN{n}:PROB {}", ch.probe_gain))?;
        self.set_channel_enabled(s, n, ch.enabled)?;
        s.write(&format!(":CHAN{n}:COUP {}", ch.coupling))?;
        self.set_termination_ohms(s, n, ch.termination_ohms)?;
        self.set_bandwidth_hz(s, n, ch.bandwidth_hz)?;
        s.write(&format!(":CHAN{n}:SCAL {}", ch.scale))?;
        s.write(&format!(":CHAN{n}:OFFS {}", ch.offset))
    }

    fn apply_horizontal(&self, s: &mut ScpiSession, h: &HorizontalConfig) -> Result<(), ScpiError> {
        if h.record_length > 0 {
            s.write(&format!(":ACQ:MDEP {}", h.record_length))?;
        }
        s.write(&format!(":TIM:MAIN:SCAL {}", h.scale))
    }

    fn apply_trigger(&self, s: &mut ScpiSession, t: &TriggerConfig) -> Result<(), ScpiError> {
        s.write(":TRIG:MODE EDGE")?;
        s.write(&format!(":TRIG:SWE {}", t.mode))?;
        s.write(&format!(":TRIG:EDGE:SOUR {}", source_to_write(&t.source)))?;
        s.write(&format!(":TRIG:EDGE:SLOP {}", slope_to_write(&t.slope)))?;
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
            s.write(":RUNing OFF")
        } else if stop_after.eq_ignore_ascii_case("SEQUENCE") {
            s.write(":SINGle")
        } else {
            s.write(":RUNing ON")
        }
    }

    fn fetch_channel(&self, s: &mut ScpiSession, ch: &str) -> Result<ChannelTrace, WaveformError> {
        let n = channel_number(ch)
            .ok_or_else(|| WaveformError::Parse(format!("{} cannot source {ch}", self.name)))?;

        let probe = self.probe_ratio(s, n);
        let block = s.query_header_block(
            &format!(":WAVeform:DATA:DISP? Channel{n}"),
            HEADER_BYTES,
            HEADER_LEN_OFFSET,
            HEADER_LEN_DIGITS,
        )?;
        let header = Header::parse(&block, n).ok_or_else(|| {
            WaveformError::Parse(format!(
                "{} byte header from :WAVeform:DATA:DISP? is too short",
                block.len()
            ))
        })?;

        let sample_rate = header.sample_rate;
        let points = block[HEADER_BYTES..]
            .iter()
            .enumerate()
            .map(|(i, code)| {
                let seconds = if sample_rate > 0.0 {
                    i as f64 / sample_rate
                } else {
                    0.0
                };
                [
                    seconds - header.pretrigger_s,
                    (header.scale_v * probe / 24.0) * (*code as f64 - 128.0)
                        - header.offset_v * probe,
                ]
            })
            .collect();

        Ok(ChannelTrace {
            channel: format!("CH{n}"),
            x_unit: "s".into(),
            y_unit: "V".into(),
            points,
        })
    }

    /// `:SINGle` arms one acquisition; `:TRIG:STAT?` reads `STOP` once it lands.
    fn wait_sequence(&self, s: &mut ScpiSession, timeout: Duration) -> Result<(), ScpiError> {
        s.write(":SINGle")?;
        let start = Instant::now();
        loop {
            thread::sleep(Duration::from_millis(100));
            if self.stopped(s)? {
                return Ok(());
            }
            if start.elapsed() > timeout {
                let _ = s.write(":RUNing OFF");
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
        s.write(":AUToscale")
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn source_to_write(source: &str) -> String {
    match channel_number(source) {
        Some(n) => format!("CHANnel{n}"),
        None => source.to_ascii_uppercase(),
    }
}

fn source_to_read(reply: &str) -> String {
    let upper = reply.trim().to_ascii_uppercase();
    match channel_number(&upper) {
        Some(n) => format!("CH{n}"),
        None => upper,
    }
}

fn slope_to_write(slope: &str) -> &str {
    match slope {
        "FALL" => "FALLing",
        "EITHER" => "EITHer",
        _ => "RISIng",
    }
}

fn slope_to_read(reply: &str) -> String {
    match reply.trim().to_ascii_uppercase().as_str() {
        "FALL" => "FALL".into(),
        "EITH" => "EITHER".into(),
        _ => "RISE".into(),
    }
}

fn acquire_to_write(mode: &str) -> &str {
    match mode {
        "SAMPLE" => "NORMal",
        "PEAKDETECT" => "PEAK",
        "AVERAGE" => "AVERages",
        "ULTRA" | "HIRES" => "HRESolution",
        other => other,
    }
}

fn acquire_to_read(reply: &str) -> String {
    let upper = reply.trim().to_ascii_uppercase();
    match upper.as_str() {
        "NORM" => "SAMPLE".into(),
        "PEAK" => "PEAKDETECT".into(),
        "AVERAGE" | "AVER" => "AVERAGE".into(),
        "HRESOLUTION" | "HRES" => "ULTRA".into(),
        other => other.to_string(),
    }
}

/// The scaling fields of the 128-byte `:WAVeform:DATA:*?` header.
#[derive(Debug, Clone, PartialEq)]
struct Header {
    /// Vertical scale (V/div) of the requested channel, probe ratio excluded.
    scale_v: f64,
    /// Vertical offset of the requested channel, in volts.
    offset_v: f64,
    sample_rate: f64,
    /// Time from the first sample to the trigger, in seconds.
    pretrigger_s: f64,
    running: bool,
    triggered: bool,
}

impl Header {
    /// Parse the fields this driver needs for channel `n` (1-based).
    fn parse(block: &[u8], n: usize) -> Option<Self> {
        if block.len() < HEADER_BYTES {
            return None;
        }
        let offset_v = read_i32_le(block, 31 + 4 * (n - 1))? as f64 / 1e6;
        let scale_v = ascii_f64(block.get(47 + 7 * (n - 1)..54 + 7 * (n - 1))?)?;
        let sample_rate = ascii_f64(block.get(79..88)?)?;
        let horizontal_offset_ps = read_i64_le(block, 94)?;
        let pretrigger_ps = read_i64_le(block, 103)?;
        // The pre-trigger field is the useful reference for time zero; the
        // horizontal offset only shifts the view and is added on top of it.
        let pretrigger_s = (pretrigger_ps as f64 + horizontal_offset_ps as f64) / 1e12;
        Some(Self {
            scale_v,
            offset_v,
            sample_rate,
            pretrigger_s,
            running: block[29] == b'1',
            triggered: block[30] != b'0',
        })
    }
}

/// Read a little-endian 32-bit field out of the header.
fn read_i32_le(block: &[u8], offset: usize) -> Option<i32> {
    Some(i32::from_le_bytes(
        block.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

/// Read a little-endian 64-bit field out of the header.
fn read_i64_le(block: &[u8], offset: usize) -> Option<i64> {
    Some(i64::from_le_bytes(
        block.get(offset..offset + 8)?.try_into().ok()?,
    ))
}

/// Parse a fixed-width, space- or NUL-padded ASCII number.
fn ascii_f64(field: &[u8]) -> Option<f64> {
    let text: String = field
        .iter()
        .map(|byte| *byte as char)
        .filter(|c| !c.is_control())
        .collect();
    text.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a header the way the manual describes it.
    fn header_bytes(scale_v: &str, sample_rate: &str, pretrigger_ps: i64) -> Vec<u8> {
        let put = |block: &mut Vec<u8>, at: usize, width: usize, text: &str| {
            let mut field = vec![b' '; width];
            let bytes = text.as_bytes();
            let len = bytes.len().min(width);
            field[..len].copy_from_slice(&bytes[..len]);
            block[at..at + width].copy_from_slice(&field);
        };
        let mut block = vec![0u8; HEADER_BYTES + 4];
        block[0] = b'#';
        block[1] = b'9';
        block[11..20].copy_from_slice(b"000000004"); // payload byte count
        block[29] = b'0'; // stopped
        block[30] = b'1'; // triggered
        block[31..35].copy_from_slice(&(-1_000_000i32).to_le_bytes()); // ch1 offset, µV
        put(&mut block, 47, 7, scale_v);
        put(&mut block, 54, 7, "1.000");
        put(&mut block, 79, 9, sample_rate);
        block[94..102].copy_from_slice(&0i64.to_le_bytes());
        block[103..111].copy_from_slice(&pretrigger_ps.to_le_bytes());
        block[128..].copy_from_slice(&[128u8, 129, 127, 200]);
        block
    }

    #[test]
    fn derives_name_and_bandwidth_from_idn() {
        let scope = Hrdo2000::from_idn("Hantek,HRDO2204,SN,202606");
        assert_eq!(scope.name(), "Hantek HRDO2204");
        // No bandwidth digits in the model: the series default.
        assert_eq!(scope.full_bandwidth_hz, 200e6);
        assert_eq!(
            Hrdo2000::from_idn("Hantek,HRDO2350X,X,1").full_bandwidth_hz,
            350e6
        );
        assert_eq!(
            Hrdo2000::from_idn("Hantek,HRDO2100X,X,1").full_bandwidth_hz,
            100e6
        );
    }

    #[test]
    fn parses_the_binary_header() {
        let block = header_bytes("1.000", "1.000e9", 1_000);
        let header = Header::parse(&block, 1).unwrap();
        assert_eq!(header.scale_v, 1.0);
        assert_eq!(header.offset_v, -1.0);
        assert_eq!(header.sample_rate, 1e9);
        assert!((header.pretrigger_s - 1e-9).abs() < 1e-18);
        assert!(!header.running);
        assert!(header.triggered);
    }

    #[test]
    fn scales_samples_like_the_manual() {
        let block = header_bytes("2.400", "1.000e6", 0);
        let header = Header::parse(&block, 1).unwrap();
        // Header: 2.4 V/div, -1 V offset, probe 10x -> 1 V per 24 codes.
        let volts =
            |code: f64| (header.scale_v * 10.0 / 24.0) * (code - 128.0) - header.offset_v * 10.0;
        // code 128 is mid-scale: 0 V minus the -1 V offset times the probe.
        assert!((volts(128.0) - 10.0).abs() < 1e-9);
        // 20 codes below mid-scale is -20 V, plus the same 10 V offset.
        assert!((volts(108.0) - (-10.0)).abs() < 1e-9);
        assert!((volts(104.0) - (-14.0)).abs() < 1e-9);
    }

    #[test]
    fn rejects_short_headers() {
        assert!(Header::parse(&[0u8; 64], 1).is_none());
    }

    #[test]
    fn maps_slopes_and_acquisition_modes() {
        assert_eq!(slope_to_write("RISE"), "RISIng");
        assert_eq!(slope_to_write("FALL"), "FALLing");
        assert_eq!(slope_to_read("EITH"), "EITHER");
        assert_eq!(acquire_to_write("SAMPLE"), "NORMal");
        assert_eq!(acquire_to_write("ULTRA"), "HRESolution");
        assert_eq!(acquire_to_read("AVERAge"), "AVERAGE");
        assert_eq!(acquire_to_read("HRESolution"), "ULTRA");
    }
}
