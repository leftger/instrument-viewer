use std::time::Duration;

use crate::scpi::{ScpiError, ScpiSession};
use crate::waveform::{ChannelTrace, WaveformError};

/// The instrument-specific half of the SCPI surface.
///
/// A Rigol DHO900 answers a useful subset of the Tektronix command set —
/// `HORIZONTAL:*`, `ACQUIRE:*`, `TRIGGER:A:EDGE:*` and most of `CH<n>:*` all
/// work verbatim, in both directions — so those stay in `config.rs`. This trait
/// covers only what the two instruments disagree about. Unsupported commands
/// return nothing at all on a Rigol and push `-100,"Command err"` onto its error
/// queue, so the difference surfaces as a read timeout rather than an error.
pub trait Backend: Send {
    fn name(&self) -> &str;

    /// Commands to re-assert on every `ScpiSession::resync`.
    fn preamble(&self) -> Vec<String> {
        Vec::new()
    }

    fn channel_enabled(&self, s: &mut ScpiSession, n: usize) -> Result<bool, ScpiError>;
    fn set_channel_enabled(&self, s: &mut ScpiSession, n: usize, on: bool)
        -> Result<(), ScpiError>;

    fn termination_ohms(&self, s: &mut ScpiSession, n: usize) -> Result<f64, ScpiError>;
    fn set_termination_ohms(
        &self,
        s: &mut ScpiSession,
        n: usize,
        ohms: f64,
    ) -> Result<(), ScpiError>;

    fn bandwidth_hz(&self, s: &mut ScpiSession, n: usize) -> Result<f64, ScpiError>;
    fn set_bandwidth_hz(&self, s: &mut ScpiSession, n: usize, hz: f64) -> Result<(), ScpiError>;

    fn probe_type(&self, s: &mut ScpiSession, n: usize) -> Result<String, ScpiError>;

    fn trigger_level(&self, s: &mut ScpiSession, source: &str) -> Result<f64, ScpiError>;
    fn set_trigger_level(
        &self,
        s: &mut ScpiSession,
        source: &str,
        volts: f64,
    ) -> Result<(), ScpiError>;

    fn apply_acquisition(
        &self,
        s: &mut ScpiSession,
        mode: &str,
        stop_after: &str,
        running: bool,
    ) -> Result<(), ScpiError> {
        s.write(&format!("ACQUIRE:MODE {mode}"))?;
        s.write(&format!("ACQUIRE:STOPAFTER {stop_after}"))?;
        s.write(&format!(
            "ACQUIRE:STATE {}",
            if running { "ON" } else { "OFF" }
        ))
    }

    fn fetch_channel(&self, s: &mut ScpiSession, ch: &str) -> Result<ChannelTrace, WaveformError>;

    /// Arm a single acquisition and wait for it to complete.
    fn wait_sequence(&self, s: &mut ScpiSession, timeout: Duration) -> Result<(), ScpiError>;

    fn autoset(&self, s: &mut ScpiSession) -> Result<(), ScpiError>;
}

/// Pick a backend from the `*IDN?` response, defaulting to Tektronix.
pub fn from_idn(idn: &str) -> Box<dyn Backend> {
    if idn.to_ascii_uppercase().contains("RIGOL") {
        Box::new(crate::rigol::Rigol::from_idn(idn))
    } else {
        Box::new(crate::tek::Tek)
    }
}

/// `CH1` / `1` / `CHAN1` to a channel index.
pub fn channel_number(ch: &str) -> Option<usize> {
    let digits: String = ch.chars().filter(|c| c.is_ascii_digit()).collect();
    match digits.parse() {
        Ok(n @ 1..=4) => Some(n),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_backend_by_vendor() {
        assert_eq!(
            from_idn("RIGOL TECHNOLOGIES,DHO924S,SN,00.01.05").name(),
            "Rigol DHO924S"
        );
        assert_eq!(
            from_idn("TEKTRONIX,MDO3024,B020857,CF:91.1CT FV:v1.30").name(),
            "Tektronix"
        );
        // An unreadable IDN must not lose the historically supported instrument.
        assert_eq!(from_idn("").name(), "Tektronix");
    }

    #[test]
    fn parses_channel_numbers() {
        assert_eq!(channel_number("CH1"), Some(1));
        assert_eq!(channel_number("CHAN4"), Some(4));
        assert_eq!(channel_number("3"), Some(3));
        assert_eq!(channel_number("CH5"), None);
        assert_eq!(channel_number("MATH"), None);
    }
}
