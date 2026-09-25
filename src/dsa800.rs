//! Rigol DSA800 series spectrum analyzer (DSA815, DSA832, DSA875).
//!
//! The swept-trace model maps onto the app's existing plot as a single
//! frequency-domain trace: center/span live in the horizontal settings
//! (`position` = center Hz, `scale` = span Hz) and a fetch reads the 601-point
//! trace with `:TRACe:DATA? TRACE1` plus the start/stop frequencies for the
//! x axis. Amplitude is left in the analyzer's native dBm.

use std::time::Duration;

use crate::backend::{
    AcquisitionStatus, Backend, InstrumentCapabilities, InstrumentKind, ValueChoice,
};
use crate::config::{
    AcquisitionConfig, ChannelConfig, HorizontalConfig, InstrumentConfig, TriggerConfig,
};
use crate::scpi::{ScpiError, ScpiSession};
use crate::waveform::{ChannelTrace, WaveformError};

/// Sweep points of the DSA800's standard trace.
const TRACE_POINTS: u64 = 601;

/// Rigol DSA800 series spectrum analyzer.
pub struct Dsa800 {
    name: String,
}

impl Dsa800 {
    pub fn from_idn(idn: &str) -> Self {
        let model = idn
            .split(',')
            .nth(1)
            .unwrap_or("")
            .trim()
            .to_ascii_uppercase();
        Self {
            name: if model.is_empty() {
                "Rigol DSA800".to_string()
            } else {
                format!("Rigol {model}")
            },
        }
    }

    fn center_hz(&self, s: &mut ScpiSession) -> Result<f64, ScpiError> {
        crate::scpi::parse_f64(&s.query(":FREQ:CENT?")?)
    }

    fn span_hz(&self, s: &mut ScpiSession) -> Result<f64, ScpiError> {
        crate::scpi::parse_f64(&s.query(":FREQ:SPAN?")?)
    }
}

impl Backend for Dsa800 {
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
            record_lengths: vec![TRACE_POINTS],
            trigger_modes: vec!["NONE".into()],
            trigger_slopes: vec!["—".into()],
            trigger_couplings: vec!["—".into()],
            acquisition_modes: vec!["SWEEP".into()],
            stop_after: vec!["RUNSTOP".into()],
            channel_hint: Some("RF input is 50 Ω; amplitude is in dBm.".into()),
            horizontal_hint: Some(
                "Center (Hz) and span (Hz) sweep the analyzer; traces are 601 points.".into(),
            ),
            acquisition_hint: Some("Fetch reads TRACE1 in the analyzer's native dBm.".into()),
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
                scale: self.span_hz(s)?,
                position: self.center_hz(s)?,
                record_length: TRACE_POINTS,
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
        // ASCII keeps the transfer simple and the block parser already strips
        // the #9 header; each point is a comma-separated scientific float.
        s.write(":FORMat ASC")?;
        let block = s.query_binary_block(":TRACe:DATA? TRACE1")?;
        let text = std::str::from_utf8(&block)
            .map_err(|_| WaveformError::Parse("TRACE1 data is not ASCII".into()))?;
        let values: Vec<f64> = text
            .split(',')
            .filter_map(|part| part.trim().parse::<f64>().ok())
            .collect();
        if values.is_empty() {
            return Err(WaveformError::Parse(
                "TRACE1 returned no numeric data".into(),
            ));
        }

        let start = crate::scpi::parse_f64(&s.query(":FREQ:STAR?")?)?;
        let stop = crate::scpi::parse_f64(&s.query(":FREQ:STOP?")?)?;
        let n = values.len();
        let span = stop - start;
        let points = values
            .into_iter()
            .enumerate()
            .map(|(i, dbm)| {
                let hz = if n > 1 {
                    start + span * (i as f64) / ((n - 1) as f64)
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
            Dsa800::from_idn("RIGOL TECHNOLOGIES,DSA815,DSA8A123456789,00.01.17").name(),
            "Rigol DSA815"
        );
        assert_eq!(Dsa800::from_idn("").name(), "Rigol DSA800");
    }
}
