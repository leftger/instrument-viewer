//! Siglent SSA/SVA/SHA spectrum analyzers.
//!
//! **Best-effort driver.** The manual shipped with this project is an IVI-C
//! driver guide (`ssa_ConfigureFrequencyCenterSpan`, `SSA_ATTR_CENTER_FREQUENCY`,
//! …) and documents no SCPI strings, so the commands here come from the
//! publicly documented SSA3000X command set rather than from a manual in
//! `docs/`. Treat a first connection as a probe: the center/span/start/stop
//! queries are tolerant of missing replies, and the trace parser accepts either
//! ASCII or `REAL,32` blocks and both byte orders.

use std::time::Duration;

use crate::backend::{
    AcquisitionStatus, Backend, InstrumentCapabilities, InstrumentKind, ValueChoice,
};
use crate::config::{
    AcquisitionConfig, ChannelConfig, HorizontalConfig, InstrumentConfig, TriggerConfig,
};
use crate::scpi::{ScpiError, ScpiSession};
use crate::waveform::{ChannelTrace, WaveformError};

/// Sweep points to assume when the instrument does not report them.
const DEFAULT_POINTS: u64 = 601;

/// Siglent SSA/SVA/SHA spectrum analyzer.
pub struct SiglentSsa {
    name: String,
}

impl SiglentSsa {
    pub fn from_idn(idn: &str) -> Self {
        let model = idn
            .split(',')
            .nth(1)
            .unwrap_or("")
            .trim()
            .to_ascii_uppercase();
        Self {
            name: if model.is_empty() {
                "Siglent SSA/SVA".to_string()
            } else {
                format!("Siglent {model}")
            },
        }
    }

    fn center_hz(&self, s: &mut ScpiSession) -> Result<f64, ScpiError> {
        crate::scpi::parse_f64(&s.query(":FREQ:CENT?")?)
    }

    fn span_hz(&self, s: &mut ScpiSession) -> Result<f64, ScpiError> {
        crate::scpi::parse_f64(&s.query(":FREQ:SPAN?")?)
    }

    /// Sweep points, when the analyzer answers the query.
    fn sweep_points(&self, s: &mut ScpiSession) -> u64 {
        crate::scpi::parse_count(&s.query(":SWE:POIN?").unwrap_or_default())
            .unwrap_or(DEFAULT_POINTS)
    }

    /// Start/stop frequencies, falling back to center ± span/2.
    fn frequency_range(&self, s: &mut ScpiSession) -> (f64, f64) {
        if let (Ok(start), Ok(stop)) = (
            crate::scpi::parse_f64(&s.query(":FREQ:STAR?").unwrap_or_default()),
            crate::scpi::parse_f64(&s.query(":FREQ:STOP?").unwrap_or_default()),
        ) {
            if stop > start {
                return (start, stop);
            }
        }
        let center = self.center_hz(s).unwrap_or(1e9);
        let span = self.span_hz(s).unwrap_or(1e6);
        (center - span / 2.0, center + span / 2.0)
    }
}

/// Decode a trace block that may be ASCII or 32-bit floats, either byte order.
fn decode_trace(block: &[u8]) -> Vec<f64> {
    if let Some(values) = decode_ascii(block) {
        return values;
    }
    // REAL,32: accept whichever byte order lands the samples in a plausible
    // dBm window; NORMal (most significant byte first) is the documented default.
    for big_endian in [true, false] {
        let values: Vec<f64> = block
            .as_chunks::<4>()
            .0
            .iter()
            .map(|chunk| {
                let value = if big_endian {
                    f32::from_be_bytes(*chunk)
                } else {
                    f32::from_le_bytes(*chunk)
                };
                value as f64
            })
            .collect();
        if plausible(&values) {
            return values;
        }
    }
    Vec::new()
}

fn decode_ascii(block: &[u8]) -> Option<Vec<f64>> {
    let text = std::str::from_utf8(block).ok()?;
    let values: Vec<f64> = text
        .split(',')
        .filter_map(|part| part.trim().parse::<f64>().ok())
        .collect();
    plausible(&values).then_some(values)
}

/// A spectrum trace in dBm lives in a narrow window; this rejects the byte
/// soup a mis-decoded REAL,32 block (or a rejected command) would produce.
/// Zero is allowed (0 dBm is a real reading); tiny denormals are not.
fn plausible(values: &[f64]) -> bool {
    !values.is_empty()
        && values.iter().all(|value| {
            value.is_finite()
                && (-400.0..=400.0).contains(value)
                && (*value == 0.0 || value.abs() >= 1e-3)
        })
}

impl Backend for SiglentSsa {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> InstrumentKind {
        InstrumentKind::Spectrum
    }

    fn capabilities(&self) -> InstrumentCapabilities {
        InstrumentCapabilities {
            channel_couplings: vec!["DC".into()],
            terminations: vec![ValueChoice::new("50 Ω", 50.0)],
            termination_writable: false,
            bandwidths: vec![ValueChoice::new("N/A", 0.0)],
            record_lengths: vec![501, 601, 751, 1001, 2001],
            trigger_modes: vec!["NONE".into()],
            trigger_slopes: vec!["—".into()],
            trigger_couplings: vec!["—".into()],
            acquisition_modes: vec!["SWEEP".into()],
            stop_after: vec!["RUNSTOP".into()],
            channel_hint: Some("RF input is 50 Ω; amplitude is in dBm.".into()),
            horizontal_hint: Some("Center (Hz) and span (Hz); sweeps are 501-2001 points.".into()),
            acquisition_hint: Some(
                "Best-effort SCPI: see the driver notes; TRACE1 is read in ASCII or REAL,32."
                    .into(),
            ),
            kind: InstrumentKind::Spectrum,
            channel_count: 1,
            wave_types: Vec::new(),
            output_pairs: Vec::new(),
        }
    }

    fn read_config(&self, s: &mut ScpiSession) -> Result<InstrumentConfig, ScpiError> {
        Ok(InstrumentConfig {
            channels: vec![ChannelConfig {
                enabled: true,
                scale: 0.0,
                position: 0.0,
                offset: 0.0,
                coupling: "DC".into(),
                termination_ohms: 50.0,
                bandwidth_hz: 0.0,
                probe_gain: 1.0,
                probe_type: "—".into(),
                wave_type: String::new(),
                frequency_hz: 0.0,
            }],
            horizontal: HorizontalConfig {
                scale: self.span_hz(s).unwrap_or(1e6),
                position: self.center_hz(s).unwrap_or(1e9),
                record_length: self.sweep_points(s),
            },
            trigger: TriggerConfig {
                mode: "NONE".into(),
                source: "CH1".into(),
                slope: "—".into(),
                coupling: "—".into(),
                level: 0.0,
            },
            acquisition: AcquisitionConfig {
                mode: "SWEEP".into(),
                stop_after: "RUNSTOP".into(),
                running: true,
            },
            output_pair: String::new(),
        })
    }

    fn apply_channel(
        &self,
        _s: &mut ScpiSession,
        _n: usize,
        _ch: &ChannelConfig,
    ) -> Result<(), ScpiError> {
        Ok(())
    }

    fn apply_horizontal(&self, s: &mut ScpiSession, h: &HorizontalConfig) -> Result<(), ScpiError> {
        s.write(&format!(":FREQ:CENT {}", h.position))?;
        s.write(&format!(":FREQ:SPAN {}", h.scale))
    }

    fn apply_trigger(&self, _s: &mut ScpiSession, _t: &TriggerConfig) -> Result<(), ScpiError> {
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

    fn fetch_channel(&self, s: &mut ScpiSession, _ch: &str) -> Result<ChannelTrace, WaveformError> {
        // Ask for ASCII, but stay useful if the model only does REAL,32.
        let _ = s.write(":FORMat:TRACe:DATA ASCi");
        let block = s.query_binary_block(":TRACe:DATA? TRACE1")?;
        let values = decode_trace(&block);
        if values.is_empty() {
            return Err(WaveformError::Parse(
                "TRACE1 did not decode as ASCII or REAL,32 floats".into(),
            ));
        }

        let (start, stop) = self.frequency_range(s);
        let n = values.len();
        let points = values
            .into_iter()
            .enumerate()
            .map(|(i, dbm)| {
                let hz = if n > 1 {
                    start + (stop - start) * (i as f64) / ((n - 1) as f64)
                } else {
                    start
                };
                [hz, dbm]
            })
            .collect();

        Ok(ChannelTrace {
            channel: "TRACE1".into(),
            x_unit: "Hz".into(),
            y_unit: "dBm".into(),
            points,
        })
    }

    fn fetch_channels(
        &self,
        s: &mut ScpiSession,
        _channels: &[String],
    ) -> Result<Vec<ChannelTrace>, WaveformError> {
        Ok(vec![self.fetch_channel(s, "TRACE1")?])
    }

    fn wait_sequence(&self, _s: &mut ScpiSession, _timeout: Duration) -> Result<(), ScpiError> {
        Ok(())
    }

    fn acquisition_status(&self, s: &mut ScpiSession) -> Result<AcquisitionStatus, ScpiError> {
        let center = self.center_hz(s)?;
        Ok(AcquisitionStatus {
            running: true,
            display: format!("center {center:.6e} Hz"),
        })
    }

    fn autoset(&self, _s: &mut ScpiSession) -> Result<(), ScpiError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_name_from_idn() {
        assert_eq!(
            SiglentSsa::from_idn("Siglent Technologies,SSA3021X,SSA3A123456789,1.3.9.8").name(),
            "Siglent SSA3021X"
        );
        assert_eq!(
            SiglentSsa::from_idn("Siglent Technologies,SVA1032X,SN,3.2.2.5").name(),
            "Siglent SVA1032X"
        );
        assert_eq!(SiglentSsa::from_idn("").name(), "Siglent SSA/SVA");
    }

    #[test]
    fn decodes_ascii_traces() {
        let block = b"-13.90530,-71.08871,-70.89631";
        assert_eq!(decode_trace(block), vec![-13.9053, -71.08871, -70.89631]);
    }

    #[test]
    fn decodes_big_endian_real32_traces() {
        let mut block = Vec::new();
        for value in [-20.0f32, -60.5, -80.25] {
            block.extend_from_slice(&value.to_be_bytes());
        }
        assert_eq!(decode_trace(&block), vec![-20.0, -60.5, -80.25]);
    }

    #[test]
    fn rejects_implausible_data() {
        // Four bytes that are neither ASCII nor plausible dBm values.
        assert!(decode_trace(&[0x00, 0x01, 0x02, 0x03]).is_empty());
        assert!(decode_trace(b"").is_empty());
    }
}
