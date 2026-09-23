use std::thread;
use std::time::{Duration, Instant};

use crate::backend::Backend;
use crate::config::query_f64;
use crate::scpi::{ScpiError, ScpiSession};
use crate::waveform::{self, ChannelTrace, WaveformError};

/// Tektronix MDO3000. Verified against `TEKTRONIX,MDO3024,B020857,CF:91.1CT FV:v1.30`.
pub struct Tek;

impl Backend for Tek {
    fn name(&self) -> &str {
        "Tektronix"
    }

    fn preamble(&self) -> Vec<String> {
        // HEADER OFF strips the command echo from every reply; VERBOSE OFF picks
        // the short enum spellings. Both are required for the parsers here.
        vec!["HEADER OFF".into(), "VERBOSE OFF".into()]
    }

    fn channel_enabled(&self, s: &mut ScpiSession, n: usize) -> Result<bool, ScpiError> {
        crate::config::query_bool(s, &format!("SELECT:CH{n}?"))
    }

    fn set_channel_enabled(
        &self,
        s: &mut ScpiSession,
        n: usize,
        on: bool,
    ) -> Result<(), ScpiError> {
        s.write(&format!("SELECT:CH{n} {}", if on { "ON" } else { "OFF" }))
    }

    fn termination_ohms(&self, s: &mut ScpiSession, n: usize) -> Result<f64, ScpiError> {
        query_f64(s, &format!("CH{n}:TERMINATION?"))
    }

    fn set_termination_ohms(
        &self,
        s: &mut ScpiSession,
        n: usize,
        ohms: f64,
    ) -> Result<(), ScpiError> {
        s.write(&format!("CH{n}:TERMINATION {ohms}"))
    }

    fn bandwidth_hz(&self, s: &mut ScpiSession, n: usize) -> Result<f64, ScpiError> {
        query_f64(s, &format!("CH{n}:BANDWIDTH?"))
    }

    fn set_bandwidth_hz(&self, s: &mut ScpiSession, n: usize, hz: f64) -> Result<(), ScpiError> {
        s.write(&format!("CH{n}:BANDWIDTH {hz}"))
    }

    fn probe_type(&self, s: &mut ScpiSession, n: usize) -> Result<String, ScpiError> {
        Ok(s.query(&format!("CH{n}:PROBE:ID:TYPE?"))?
            .trim_matches('"')
            .to_string())
    }

    fn trigger_level(&self, s: &mut ScpiSession, source: &str) -> Result<f64, ScpiError> {
        query_f64(s, &format!("TRIGGER:A:LEVEL:{source}?"))
    }

    fn set_trigger_level(
        &self,
        s: &mut ScpiSession,
        source: &str,
        volts: f64,
    ) -> Result<(), ScpiError> {
        s.write(&format!("TRIGGER:A:LEVEL:{source} {volts}"))
    }

    fn fetch_channel(&self, s: &mut ScpiSession, ch: &str) -> Result<ChannelTrace, WaveformError> {
        waveform::fetch_channel(s, ch)
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

    fn autoset(&self, s: &mut ScpiSession) -> Result<(), ScpiError> {
        s.write("AUTOSET EXECUTE")
    }
}
