use std::thread;
use std::time::{Duration, Instant};

use crate::backend::{AcquisitionStatus, Backend, InstrumentCapabilities, ValueChoice};
use crate::profile::{CommandTable, Parse, Setting, WaveformFormat};
use crate::scpi::{ScpiError, ScpiSession};
use crate::waveform::{self, ChannelTrace, WaveformError};

/// Tektronix MDO3000. Verified against `TEKTRONIX,MDO3024,B020857,CF:91.1CT FV:v1.30`.
pub struct Tek;

impl Backend for Tek {
    fn name(&self) -> &str {
        "Tektronix"
    }

    fn capabilities(&self) -> InstrumentCapabilities {
        InstrumentCapabilities {
            channel_couplings: strings(&["DC", "AC", "DCREJECT"]),
            terminations: vec![
                ValueChoice::new("50 Ω", 50.0),
                ValueChoice::new("1 MΩ", 1e6),
            ],
            termination_writable: true,
            bandwidths: vec![
                ValueChoice::new("20 MHz", 20e6),
                ValueChoice::new("100 MHz", 100e6),
                ValueChoice::new("200 MHz / Full", 200e6),
            ],
            record_lengths: vec![1_000, 10_000, 100_000, 1_000_000, 5_000_000, 10_000_000],
            trigger_modes: strings(&["AUTO", "NORMAL"]),
            trigger_slopes: strings(&["RISE", "FALL", "EITHER"]),
            trigger_couplings: strings(&["DC", "AC", "HFREJ", "LFREJ", "NOISEREJ"]),
            acquisition_modes: strings(&["SAMPLE", "PEAKDETECT", "HIRES", "AVERAGE", "ENVELOPE"]),
            stop_after: strings(&["RUNSTOP", "SEQUENCE"]),
            channel_hint: None,
            horizontal_hint: None,
            acquisition_hint: None,
            kind: crate::backend::InstrumentKind::Oscilloscope,
            channel_count: 4,
            wave_types: Vec::new(),
            output_pairs: Vec::new(),
        }
    }

    fn preamble(&self) -> Vec<String> {
        // HEADER OFF strips the command echo from every reply; VERBOSE OFF picks
        // the short enum spellings. Both are required for the parsers here.
        vec!["HEADER OFF".into(), "VERBOSE OFF".into()]
    }

    fn command_table(&self) -> CommandTable {
        CommandTable {
            channel_enabled: Some(Setting {
                query: Some("SELECT:CH{n}?".to_string()),
                write: Some("SELECT:CH{n} {v}".to_string()),
                parse: Parse::Bool,
            }),
            termination_ohms: Some(Setting {
                query: Some("CH{n}:TERMINATION?".to_string()),
                write: Some("CH{n}:TERMINATION {v}".to_string()),
                parse: Parse::F64,
            }),
            bandwidth_hz: Some(Setting {
                query: Some("CH{n}:BANDWIDTH?".to_string()),
                write: Some("CH{n}:BANDWIDTH {v}".to_string()),
                parse: Parse::F64,
            }),
            probe_type: Some(Setting {
                query: Some("CH{n}:PROBE:ID:TYPE?".to_string()),
                write: None,
                parse: Parse::Character,
            }),
            trigger_level: Some(Setting {
                query: Some("TRIGGER:A:LEVEL:{src}?".to_string()),
                write: Some("TRIGGER:A:LEVEL:{src} {v}".to_string()),
                parse: Parse::F64,
            }),
            ..Default::default()
        }
    }

    fn waveform_format(&self) -> Option<WaveformFormat> {
        Some(WaveformFormat::TEK)
    }

    fn fetch_channel(&self, s: &mut ScpiSession, ch: &str) -> Result<ChannelTrace, WaveformError> {
        match self.waveform_format() {
            Some(WaveformFormat::TEK) => waveform::fetch_channel(s, ch),
            _ => Err(WaveformError::Parse(format!(
                "{} has no supported waveform format",
                self.name()
            ))),
        }
    }

    /// Restores the previous `STOPAFTER` setting and leaves the scope stopped so
    /// Auto/Fetch do not keep the instrument busy.
    fn wait_sequence(&self, s: &mut ScpiSession, timeout: Duration) -> Result<(), ScpiError> {
        let previous = s.query("ACQUIRE:STOPAFTER?")?;
        s.write("ACQUIRE:STOPAFTER SEQUENCE")?;
        s.write("ACQUIRE:STATE ON")?;

        let start = Instant::now();
        loop {
            thread::sleep(Duration::from_millis(200));
            let state = s.query("ACQUIRE:STATE?")?;
            if !matches!(state.trim(), "1" | "ON" | "RUN") {
                break;
            }
            if start.elapsed() > timeout {
                let _ = s.write(&format!("ACQUIRE:STOPAFTER {previous}"));
                return Err(ScpiError::Timeout);
            }
        }

        let _ = s.write(&format!("ACQUIRE:STOPAFTER {previous}"));
        Ok(())
    }

    fn acquisition_status(&self, s: &mut ScpiSession) -> Result<AcquisitionStatus, ScpiError> {
        let running = crate::config::query_bool(s, "ACQUIRE:STATE?")?;
        Ok(AcquisitionStatus {
            running,
            display: if running { "RUN" } else { "STOP" }.into(),
        })
    }

    fn autoset(&self, s: &mut ScpiSession) -> Result<(), ScpiError> {
        s.write("AUTOSET EXECUTE")
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}
