use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::{ChannelConfig, HorizontalConfig, InstrumentConfig, TriggerConfig};
use crate::profile::{CommandTable, WaveformFormat};
use crate::scpi::{ScpiError, ScpiSession};
use crate::waveform::{ChannelTrace, WaveformError};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
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

impl Default for InstrumentCapabilities {
    fn default() -> Self {
        Self {
            channel_couplings: vec!["DC".into()],
            terminations: vec![ValueChoice::new("1 MΩ", 1e6)],
            termination_writable: false,
            bandwidths: Vec::new(),
            record_lengths: vec![1_000],
            trigger_modes: vec!["AUTO".into()],
            trigger_slopes: vec!["RISE".into()],
            trigger_couplings: vec!["DC".into()],
            acquisition_modes: vec!["SAMPLE".into()],
            stop_after: vec!["RUNSTOP".into()],
            channel_hint: None,
            horizontal_hint: None,
            acquisition_hint: None,
            kind: InstrumentKind::Oscilloscope,
            channel_count: 1,
            wave_types: Vec::new(),
            output_pairs: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstrumentKind {
    #[default]
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

    /// Declarative command table. Backends that only differ in command
    /// spelling can implement just this and inherit the default methods below;
    /// instruments with odd reply formats keep their own overrides.
    fn command_table(&self) -> CommandTable {
        CommandTable::empty()
    }

    /// Waveform transfer format for scopes; `None` for generators and supplies
    /// whose "fetch" is a preview or a measurement rather than a transfer.
    fn waveform_format(&self) -> Option<WaveformFormat> {
        None
    }

    fn channel_enabled(&self, s: &mut ScpiSession, n: usize) -> Result<bool, ScpiError> {
        let table = self.command_table();
        let reply = crate::profile::query_channel_setting(
            s,
            self.name(),
            table.channel_enabled.as_ref(),
            n,
        )?;
        crate::scpi::parse_bool(&reply)
    }

    fn set_channel_enabled(
        &self,
        s: &mut ScpiSession,
        n: usize,
        on: bool,
    ) -> Result<(), ScpiError> {
        let table = self.command_table();
        let value = if on { "ON" } else { "OFF" };
        crate::profile::write_channel_setting(
            s,
            self.name(),
            table.channel_enabled.as_ref(),
            n,
            value,
        )
    }

    fn termination_ohms(&self, s: &mut ScpiSession, n: usize) -> Result<f64, ScpiError> {
        let table = self.command_table();
        let reply = crate::profile::query_channel_setting(
            s,
            self.name(),
            table.termination_ohms.as_ref(),
            n,
        )?;
        crate::scpi::parse_f64(&reply)
    }

    fn set_termination_ohms(
        &self,
        s: &mut ScpiSession,
        n: usize,
        ohms: f64,
    ) -> Result<(), ScpiError> {
        let table = self.command_table();
        crate::profile::write_channel_setting(
            s,
            self.name(),
            table.termination_ohms.as_ref(),
            n,
            &ohms.to_string(),
        )
    }

    fn bandwidth_hz(&self, s: &mut ScpiSession, n: usize) -> Result<f64, ScpiError> {
        let table = self.command_table();
        let reply =
            crate::profile::query_channel_setting(s, self.name(), table.bandwidth_hz.as_ref(), n)?;
        crate::scpi::parse_f64(&reply)
    }

    fn set_bandwidth_hz(&self, s: &mut ScpiSession, n: usize, hz: f64) -> Result<(), ScpiError> {
        let table = self.command_table();
        crate::profile::write_channel_setting(
            s,
            self.name(),
            table.bandwidth_hz.as_ref(),
            n,
            &hz.to_string(),
        )
    }

    fn probe_type(&self, s: &mut ScpiSession, n: usize) -> Result<String, ScpiError> {
        let table = self.command_table();
        let reply =
            crate::profile::query_channel_setting(s, self.name(), table.probe_type.as_ref(), n)?;
        Ok(crate::scpi::parse_character(&reply))
    }

    fn trigger_level(&self, s: &mut ScpiSession, source: &str) -> Result<f64, ScpiError> {
        let table = self.command_table();
        let reply = crate::profile::query_source_setting(
            s,
            self.name(),
            table.trigger_level.as_ref(),
            source,
        )?;
        crate::scpi::parse_f64(&reply)
    }

    fn set_trigger_level(
        &self,
        s: &mut ScpiSession,
        source: &str,
        volts: f64,
    ) -> Result<(), ScpiError> {
        let table = self.command_table();
        crate::profile::write_source_setting(
            s,
            self.name(),
            table.trigger_level.as_ref(),
            source,
            &volts.to_string(),
        )
    }

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

    /// Read a full settings snapshot. Generators and supplies have their own
    /// state model and override this; the default handles a scope's
    /// channel/horizontal/trigger layout. The callee is generic over
    /// `Backend + ?Sized` so `self` passes through without an unsized coercion.
    fn read_config(&self, s: &mut ScpiSession) -> Result<InstrumentConfig, ScpiError> {
        crate::config::read_scope_config(s, self)
    }

    /// Apply one channel's settings. Generators and supplies override this;
    /// the default writes a scope channel's coupling/termination/bandwidth/scale.
    fn apply_channel(
        &self,
        s: &mut ScpiSession,
        n: usize,
        ch: &ChannelConfig,
    ) -> Result<(), ScpiError> {
        crate::config::apply_scope_channel(self, s, n, ch)
    }

    /// Apply the timebase. Generators and supplies have no timebase and
    /// override this as a no-op.
    fn apply_horizontal(&self, s: &mut ScpiSession, h: &HorizontalConfig) -> Result<(), ScpiError> {
        crate::config::apply_scope_horizontal(s, h)
    }

    /// Apply the edge trigger. Generators and supplies have no trigger and
    /// override this as a no-op.
    fn apply_trigger(&self, s: &mut ScpiSession, t: &TriggerConfig) -> Result<(), ScpiError> {
        crate::config::apply_scope_trigger(self, s, t)
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

/// Pick a backend from the `*IDN?` response.
///
/// Data-driven YAML profiles are consulted first, so a profile can add an
/// instrument without Rust code. Hand-written backends follow as the fallback,
/// defaulting to Tektronix.
pub fn from_idn(idn: &str) -> Box<dyn Backend> {
    if let Some(profile_backend) = crate::registry::backend_for(idn) {
        return profile_backend;
    }
    from_idn_builtin(idn)
}

/// The hand-written vendor dispatch, used when no profile matches.
fn from_idn_builtin(idn: &str) -> Box<dyn Backend> {
    let upper = idn.to_ascii_uppercase();
    // Checked before the generic SIGLENT/SDG branch below: an SDS oscilloscope's
    // IDN also contains "SIGLENT", so the scope-vs-generator split must come first.
    if upper.contains("SDS") {
        Box::new(crate::sds::Sds::from_idn(idn))
    } else if upper.contains("SIGLENT") || upper.contains(",SDG") {
        Box::new(crate::siglent::Siglent::from_idn(idn))
    } else if crate::keysight::is_power_supply_idn(&upper) {
        Box::new(crate::keysight::KeysightPsu::from_idn(idn))
    } else if upper.contains("RIGOL") {
        Box::new(crate::rigol::Rigol::from_idn(idn))
    } else if upper.contains("AFG") {
        Box::new(crate::afg::Afg::from_idn(idn))
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

        let afg = from_idn("TEKTRONIX,AFG3051C,SN,SCPI:99.0 FV:2.7");
        assert_eq!(afg.name(), "Tektronix AFG3051C");
        assert_eq!(afg.kind(), InstrumentKind::Generator);
        assert_eq!(afg.capabilities().channel_count, 1);

        let sds = from_idn("Siglent Technologies,SDS1104X-E,SDS1EBAC0L0098,7.6.1.15");
        assert_eq!(sds.name(), "Siglent SDS1104X-E");
        assert_eq!(sds.kind(), InstrumentKind::Oscilloscope);
        assert_eq!(sds.capabilities().channel_count, 4);
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
