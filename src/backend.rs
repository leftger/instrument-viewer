use std::time::Duration;

use crate::scpi::{ScpiError, ScpiSession};
use crate::waveform::{ChannelTrace, WaveformError};

#[derive(Clone, Debug, PartialEq)]
pub struct ValueChoice {
    pub label: String,
    pub value: f64,
}

impl ValueChoice {
    pub fn new(label: impl Into<String>, value: f64) -> Self {
        Self {
            label: label.into(),
            value,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct InstrumentCapabilities {
    pub channel_couplings: Vec<String>,
    pub terminations: Vec<ValueChoice>,
    pub termination_writable: bool,
    pub bandwidths: Vec<ValueChoice>,
    pub record_lengths: Vec<u64>,
    pub trigger_modes: Vec<String>,
    pub trigger_slopes: Vec<String>,
    pub trigger_couplings: Vec<String>,
    pub acquisition_modes: Vec<String>,
    pub stop_after: Vec<String>,
    pub channel_hint: Option<String>,
    pub horizontal_hint: Option<String>,
    pub acquisition_hint: Option<String>,
    pub kind: InstrumentKind,
    pub channel_count: usize,
    pub wave_types: Vec<String>,
    /// Dual-output supply coupling: `OFF`, `PARALLEL`, or `SERIES`. Empty when
    /// the instrument has no pairing.
    pub output_pairs: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstrumentKind {
    Oscilloscope,
    Generator,
    Supply,
}

impl InstrumentKind {
    pub fn is_scope(self) -> bool {
        matches!(self, Self::Oscilloscope)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AcquisitionStatus {
    pub running: bool,
    pub display: String,
}

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
    fn capabilities(&self) -> InstrumentCapabilities;

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

    /// Fetch all requested channels. Most instruments produce one trace per
    /// channel; supplies override this to return both voltage and current.
    fn fetch_channels(
        &self,
        s: &mut ScpiSession,
        channels: &[String],
    ) -> Result<Vec<ChannelTrace>, WaveformError> {
        channels
            .iter()
            .map(|channel| self.fetch_channel(s, channel))
            .collect()
    }

    /// Arm a single acquisition and wait for it to complete.
    fn wait_sequence(&self, s: &mut ScpiSession, timeout: Duration) -> Result<(), ScpiError>;

    fn acquisition_status(&self, s: &mut ScpiSession) -> Result<AcquisitionStatus, ScpiError>;

    fn autoset(&self, s: &mut ScpiSession) -> Result<(), ScpiError>;

    fn kind(&self) -> InstrumentKind {
        InstrumentKind::Oscilloscope
    }
}

/// Pick a backend from the `*IDN?` response, defaulting to Tektronix.
pub fn from_idn(idn: &str) -> Box<dyn Backend> {
    let upper = idn.to_ascii_uppercase();
    if upper.contains("SIGLENT") || upper.contains(",SDG") {
        Box::new(crate::siglent::Siglent::from_idn(idn))
    } else if crate::keysight::is_power_supply_idn(&upper) {
        Box::new(crate::keysight::KeysightPsu::from_idn(idn))
    } else if upper.contains("RIGOL") {
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
        let rigol = from_idn("RIGOL TECHNOLOGIES,DHO924S,SN,00.01.05");
        assert_eq!(rigol.name(), "Rigol DHO924S");
        let capabilities = rigol.capabilities();
        assert!(!capabilities.termination_writable);
        assert_eq!(capabilities.bandwidths[1].value, 250e6);
        assert!(capabilities.record_lengths.contains(&50_000_000));

        assert_eq!(
            from_idn("TEKTRONIX,MDO3024,B020857,CF:91.1CT FV:v1.30").name(),
            "Tektronix"
        );
        let siglent = from_idn("Siglent Technologies,SDG1032X,SDG1XBAX1R0001,1.01.01.33");
        assert_eq!(siglent.name(), "Siglent SDG1032X");
        assert_eq!(siglent.kind(), InstrumentKind::Generator);
        assert_eq!(siglent.capabilities().channel_count, 2);
        let psu = from_idn("Keysight Technologies,E36231A,MY1234,A.02.01.1631");
        assert_eq!(psu.name(), "Keysight E36231A");
        assert_eq!(psu.kind(), InstrumentKind::Supply);
        assert_eq!(psu.capabilities().channel_count, 1);
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
