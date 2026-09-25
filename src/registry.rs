//! Data-driven instrument registry: YAML profiles loaded at startup.
//!
//! A profile describes an instrument's `*IDN?` pattern, capabilities, command
//! templates, and waveform format as data. Built-in profiles ship with the app
//! under `profiles/`; user profiles are loaded from
//! `~/.config/instrument-viewer/profiles/*.yaml`. `backend::from_idn` consults
//! the registry before falling back to the hand-written backends, so a new
//! instrument can be added without Rust code.

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

use crate::backend::{AcquisitionStatus, Backend, InstrumentCapabilities, InstrumentKind};
use crate::config::{
    AcquisitionConfig, ChannelConfig, HorizontalConfig, InstrumentConfig, TriggerConfig,
};
use crate::profile::{CommandTable, PreambleKind, WaveformFormat};
use crate::scpi::{ScpiError, ScpiSession};
use crate::waveform::{ChannelTrace, WaveformError};

/// One instrument, described entirely as data.
#[derive(Clone, Debug, Deserialize)]
pub struct InstrumentProfile {
    /// Substrings matched case-insensitively against the `*IDN?` reply. The
    /// first matching profile wins over the hand-written backends.
    pub idn_matches: Vec<String>,
    /// Display name shown in the GUI after the backend is selected.
    pub name: String,
    /// Commands re-asserted on every session resync.
    #[serde(default)]
    pub preamble: Vec<String>,
    #[serde(default)]
    pub capabilities: InstrumentCapabilities,
    /// Command templates. Only the settings an instrument actually has need
    /// entries; everything else is hidden or read-only.
    #[serde(default)]
    pub commands: CommandTable,
    /// Waveform transfer format for scopes. Generators and supplies omit it.
    pub waveform: Option<WaveformFormat>,
    /// Optional hand-written driver for the parts a profile cannot express yet
    /// (odd reply formats, chunked transfers, per-model quirks). One of
    /// `tek`, `rigol`, `sds`, `siglent`, `afg`, `keysight`.
    #[serde(default)]
    pub driver: Option<String>,
}

/// The `Backend` implementation driven by an [`InstrumentProfile`]. When the
/// profile names a `driver`, every behavior method delegates to that driver
/// (so no quirk is lost); otherwise the generic YAML engine serves everything.
pub struct ProfileBackend {
    profile: InstrumentProfile,
    driver: Option<Box<dyn Backend>>,
}

impl ProfileBackend {
    pub fn new(profile: InstrumentProfile, idn: &str) -> Self {
        let driver = profile
            .driver
            .as_deref()
            .and_then(|name| driver_for(name, idn));
        Self { profile, driver }
    }

    fn table(&self) -> &CommandTable {
        &self.profile.commands
    }

    // --- Typed template helpers -------------------------------------------

    fn q_f64_channel(
        &self,
        s: &mut ScpiSession,
        setting: Option<&crate::profile::Setting>,
        n: usize,
    ) -> Result<f64, ScpiError> {
        let reply = crate::profile::query_channel_setting(s, self.name(), setting, n)?;
        crate::scpi::parse_f64(&reply)
    }

    fn q_bool_channel(
        &self,
        s: &mut ScpiSession,
        setting: Option<&crate::profile::Setting>,
        n: usize,
    ) -> Result<bool, ScpiError> {
        let reply = crate::profile::query_channel_setting(s, self.name(), setting, n)?;
        crate::scpi::parse_bool(&reply)
    }

    fn q_string_channel(
        &self,
        s: &mut ScpiSession,
        setting: Option<&crate::profile::Setting>,
        n: usize,
    ) -> Result<String, ScpiError> {
        let reply = crate::profile::query_channel_setting(s, self.name(), setting, n)?;
        Ok(crate::scpi::parse_character(&reply))
    }

    fn q_f64_plain(
        &self,
        s: &mut ScpiSession,
        setting: Option<&crate::profile::Setting>,
    ) -> Result<f64, ScpiError> {
        let reply = crate::profile::query_plain_setting(s, self.name(), setting)?;
        crate::scpi::parse_f64(&reply)
    }

    fn q_bool_plain(
        &self,
        s: &mut ScpiSession,
        setting: Option<&crate::profile::Setting>,
    ) -> Result<bool, ScpiError> {
        let reply = crate::profile::query_plain_setting(s, self.name(), setting)?;
        crate::scpi::parse_bool(&reply)
    }

    fn q_string_plain(
        &self,
        s: &mut ScpiSession,
        setting: Option<&crate::profile::Setting>,
    ) -> Result<String, ScpiError> {
        let reply = crate::profile::query_plain_setting(s, self.name(), setting)?;
        Ok(crate::scpi::parse_character(&reply))
    }

    fn write_channel(
        &self,
        s: &mut ScpiSession,
        setting: Option<&crate::profile::Setting>,
        n: usize,
        value: &str,
    ) -> Result<(), ScpiError> {
        crate::profile::write_channel_setting(s, self.name(), setting, n, value)
    }

    fn write_plain(
        &self,
        s: &mut ScpiSession,
        setting: Option<&crate::profile::Setting>,
        value: &str,
    ) -> Result<(), ScpiError> {
        crate::profile::write_plain_setting(s, self.name(), setting, value)
    }

    fn read_channel(&self, s: &mut ScpiSession, n: usize) -> Result<ChannelConfig, ScpiError> {
        let table = self.table();
        let kind = self.kind();
        Ok(ChannelConfig {
            enabled: table
                .channel_enabled
                .is_some()
                .then(|| self.q_bool_channel(s, table.channel_enabled.as_ref(), n))
                .transpose()?
                .unwrap_or(true),
            scale: table
                .scale
                .is_some()
                .then(|| self.q_f64_channel(s, table.scale.as_ref(), n))
                .transpose()?
                .unwrap_or(1.0),
            position: table
                .position
                .is_some()
                .then(|| self.q_f64_channel(s, table.position.as_ref(), n))
                .transpose()?
                .unwrap_or(0.0),
            offset: table
                .offset
                .is_some()
                .then(|| self.q_f64_channel(s, table.offset.as_ref(), n))
                .transpose()?
                .unwrap_or(0.0),
            coupling: table
                .coupling
                .is_some()
                .then(|| self.q_string_channel(s, table.coupling.as_ref(), n))
                .transpose()?
                .unwrap_or_else(|| "DC".into()),
            termination_ohms: table
                .termination_ohms
                .is_some()
                .then(|| self.q_f64_channel(s, table.termination_ohms.as_ref(), n))
                .transpose()?
                .unwrap_or(1e6),
            bandwidth_hz: table
                .bandwidth_hz
                .is_some()
                .then(|| self.q_f64_channel(s, table.bandwidth_hz.as_ref(), n))
                .transpose()?
                .unwrap_or_else(|| {
                    self.profile
                        .capabilities
                        .bandwidths
                        .iter()
                        .map(|c| c.value)
                        .fold(0.0, f64::max)
                }),
            probe_gain: table
                .probe_gain
                .is_some()
                .then(|| self.q_f64_channel(s, table.probe_gain.as_ref(), n))
                .transpose()?
                .unwrap_or(1.0),
            probe_type: table
                .probe_type
                .is_some()
                .then(|| self.q_string_channel(s, table.probe_type.as_ref(), n))
                .transpose()?
                .unwrap_or_else(|| "unknown".into()),
            wave_type: if kind == InstrumentKind::Generator {
                table
                    .wave_type
                    .is_some()
                    .then(|| self.q_string_channel(s, table.wave_type.as_ref(), n))
                    .transpose()?
                    .unwrap_or_else(|| "SINE".into())
            } else {
                String::new()
            },
            frequency_hz: if kind == InstrumentKind::Generator {
                table
                    .frequency_hz
                    .is_some()
                    .then(|| self.q_f64_channel(s, table.frequency_hz.as_ref(), n))
                    .transpose()?
                    .unwrap_or(1e3)
            } else {
                0.0
            },
        })
    }

    fn read_scope_horizontal(&self, s: &mut ScpiSession) -> Result<HorizontalConfig, ScpiError> {
        let table = self.table();
        Ok(HorizontalConfig {
            scale: self.q_f64_plain(s, table.horizontal_scale.as_ref())?,
            position: self.q_f64_plain(s, table.horizontal_position.as_ref())?,
            record_length: crate::scpi::parse_count(&crate::profile::query_plain_setting(
                s,
                self.name(),
                table.record_length.as_ref(),
            )?)
            .ok_or_else(|| ScpiError::Parse("record length reply is not a count".into()))?,
        })
    }

    fn read_scope_trigger(&self, s: &mut ScpiSession) -> Result<TriggerConfig, ScpiError> {
        let table = self.table();
        let source = self.q_string_plain(s, table.trigger_source.as_ref())?;
        let level = crate::scpi::parse_f64(&crate::profile::query_source_setting(
            s,
            self.name(),
            table.trigger_level.as_ref(),
            &source,
        )?)?;
        Ok(TriggerConfig {
            mode: self.q_string_plain(s, table.trigger_mode.as_ref())?,
            source,
            slope: self.q_string_plain(s, table.trigger_slope.as_ref())?,
            coupling: self.q_string_plain(s, table.trigger_coupling.as_ref())?,
            level,
        })
    }

    fn read_scope_acquisition(&self, s: &mut ScpiSession) -> Result<AcquisitionConfig, ScpiError> {
        let table = self.table();
        Ok(AcquisitionConfig {
            mode: self.q_string_plain(s, table.acquisition_mode.as_ref())?,
            stop_after: self.q_string_plain(s, table.stop_after.as_ref())?,
            running: self.q_bool_plain(s, table.running.as_ref())?,
        })
    }
}

impl Backend for ProfileBackend {
    fn name(&self) -> &str {
        self.driver
            .as_deref()
            .map(Backend::name)
            .unwrap_or(&self.profile.name)
    }

    fn kind(&self) -> InstrumentKind {
        self.driver
            .as_deref()
            .map(Backend::kind)
            .unwrap_or(self.profile.capabilities.kind)
    }

    fn capabilities(&self) -> InstrumentCapabilities {
        self.driver
            .as_deref()
            .map(Backend::capabilities)
            .unwrap_or_else(|| self.profile.capabilities.clone())
    }

    fn preamble(&self) -> Vec<String> {
        self.driver
            .as_deref()
            .map(Backend::preamble)
            .unwrap_or_else(|| self.profile.preamble.clone())
    }

    fn command_table(&self) -> CommandTable {
        self.driver
            .as_deref()
            .map(Backend::command_table)
            .unwrap_or_else(|| self.profile.commands.clone())
    }

    fn waveform_format(&self) -> Option<WaveformFormat> {
        self.driver
            .as_deref()
            .and_then(Backend::waveform_format)
            .or(self.profile.waveform)
    }

    fn read_config(&self, s: &mut ScpiSession) -> Result<InstrumentConfig, ScpiError> {
        if let Some(driver) = self.driver.as_deref() {
            return driver.read_config(s);
        }
        let nch = self.profile.capabilities.channel_count;
        let mut channels = Vec::with_capacity(nch);
        for n in 1..=nch {
            channels.push(self.read_channel(s, n)?);
        }

        let (horizontal, trigger, acquisition) = match self.kind() {
            InstrumentKind::Oscilloscope => (
                self.read_scope_horizontal(s)?,
                self.read_scope_trigger(s)?,
                self.read_scope_acquisition(s)?,
            ),
            InstrumentKind::Generator | InstrumentKind::Supply => {
                let freq = channels
                    .iter()
                    .find(|ch| ch.enabled && ch.frequency_hz > 0.0)
                    .map(|ch| ch.frequency_hz)
                    .unwrap_or_else(|| channels.first().map(|ch| ch.frequency_hz).unwrap_or(1e3))
                    .max(1.0);
                let running = channels.iter().any(|ch| ch.enabled);
                (
                    HorizontalConfig {
                        scale: if self.kind() == InstrumentKind::Generator {
                            2.0 / freq / 10.0
                        } else {
                            0.1
                        },
                        position: 50.0,
                        record_length: 2_000,
                    },
                    TriggerConfig {
                        mode: "NONE".into(),
                        source: "CH1".into(),
                        slope: "RISE".into(),
                        coupling: "DC".into(),
                        level: 0.0,
                    },
                    AcquisitionConfig {
                        mode: if self.kind() == InstrumentKind::Generator {
                            "PREVIEW".into()
                        } else {
                            "MEASURE".into()
                        },
                        stop_after: "RUNSTOP".into(),
                        running,
                    },
                )
            }
        };

        Ok(InstrumentConfig {
            channels,
            horizontal,
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
        if let Some(driver) = self.driver.as_deref() {
            return driver.apply_channel(s, n, ch);
        }
        let table = self.table();
        // Probe gain changes the engineering units of scale/offset, so set it first.
        self.write_channel(s, table.probe_gain.as_ref(), n, &ch.probe_gain.to_string())?;
        self.write_channel(
            s,
            table.channel_enabled.as_ref(),
            n,
            if ch.enabled { "ON" } else { "OFF" },
        )?;
        self.write_channel(s, table.coupling.as_ref(), n, &ch.coupling)?;
        self.write_channel(
            s,
            table.termination_ohms.as_ref(),
            n,
            &ch.termination_ohms.to_string(),
        )?;
        self.write_channel(
            s,
            table.bandwidth_hz.as_ref(),
            n,
            &ch.bandwidth_hz.to_string(),
        )?;
        self.write_channel(s, table.scale.as_ref(), n, &ch.scale.to_string())?;
        self.write_channel(s, table.position.as_ref(), n, &ch.position.to_string())?;
        self.write_channel(s, table.offset.as_ref(), n, &ch.offset.to_string())?;
        if self.kind() == InstrumentKind::Generator {
            self.write_channel(s, table.wave_type.as_ref(), n, &ch.wave_type)?;
            self.write_channel(
                s,
                table.frequency_hz.as_ref(),
                n,
                &ch.frequency_hz.to_string(),
            )?;
        }
        Ok(())
    }

    fn apply_horizontal(&self, s: &mut ScpiSession, h: &HorizontalConfig) -> Result<(), ScpiError> {
        if let Some(driver) = self.driver.as_deref() {
            return driver.apply_horizontal(s, h);
        }
        let table = self.table();
        self.write_plain(
            s,
            table.record_length.as_ref(),
            &h.record_length.to_string(),
        )?;
        self.write_plain(s, table.horizontal_scale.as_ref(), &h.scale.to_string())?;
        self.write_plain(
            s,
            table.horizontal_position.as_ref(),
            &h.position.to_string(),
        )
    }

    fn apply_trigger(&self, s: &mut ScpiSession, t: &TriggerConfig) -> Result<(), ScpiError> {
        if let Some(driver) = self.driver.as_deref() {
            return driver.apply_trigger(s, t);
        }
        let table = self.table();
        self.write_plain(s, table.trigger_mode.as_ref(), &t.mode)?;
        self.write_plain(s, table.trigger_source.as_ref(), &t.source)?;
        self.write_plain(s, table.trigger_slope.as_ref(), &t.slope)?;
        self.write_plain(s, table.trigger_coupling.as_ref(), &t.coupling)?;
        crate::profile::write_source_setting(
            s,
            self.name(),
            table.trigger_level.as_ref(),
            &t.source,
            &t.level.to_string(),
        )
    }

    fn apply_acquisition(
        &self,
        s: &mut ScpiSession,
        mode: &str,
        stop_after: &str,
        running: bool,
    ) -> Result<(), ScpiError> {
        if let Some(driver) = self.driver.as_deref() {
            return driver.apply_acquisition(s, mode, stop_after, running);
        }
        let table = self.table();
        self.write_plain(s, table.acquisition_mode.as_ref(), mode)?;
        self.write_plain(s, table.stop_after.as_ref(), stop_after)?;
        self.write_plain(
            s,
            table.running.as_ref(),
            if running { "ON" } else { "OFF" },
        )
    }

    fn fetch_channel(&self, s: &mut ScpiSession, ch: &str) -> Result<ChannelTrace, WaveformError> {
        if let Some(driver) = self.driver.as_deref() {
            return driver.fetch_channel(s, ch);
        }
        match self.profile.waveform {
            Some(WaveformFormat {
                preamble: PreambleKind::Tek,
                ..
            }) => crate::waveform::fetch_channel(s, ch),
            Some(format) => Err(WaveformError::Parse(format!(
                "{}: profile waveform {:?} fetches are not implemented yet",
                self.name(),
                format.preamble
            ))),
            None => Err(WaveformError::Parse(format!(
                "{} has no waveform transfer; use its own Fetch path",
                self.name()
            ))),
        }
    }

    fn fetch_channels(
        &self,
        s: &mut ScpiSession,
        channels: &[String],
    ) -> Result<Vec<ChannelTrace>, WaveformError> {
        if let Some(driver) = self.driver.as_deref() {
            return driver.fetch_channels(s, channels);
        }
        channels
            .iter()
            .map(|ch| self.fetch_channel(s, ch))
            .collect()
    }

    fn wait_sequence(&self, s: &mut ScpiSession, timeout: Duration) -> Result<(), ScpiError> {
        if let Some(driver) = self.driver.as_deref() {
            return driver.wait_sequence(s, timeout);
        }
        let table = self.table();
        if self.kind() != InstrumentKind::Oscilloscope {
            return Ok(());
        }
        // Tek-style single-shot expressed entirely through profile commands:
        // save STOPAFTER, arm SEQUENCE + RUN, poll until stopped, restore.
        let previous = self.q_string_plain(s, table.stop_after.as_ref())?;
        self.write_plain(s, table.stop_after.as_ref(), "SEQUENCE")?;
        self.write_plain(s, table.running.as_ref(), "ON")?;

        let start = std::time::Instant::now();
        loop {
            std::thread::sleep(Duration::from_millis(200));
            if !self.q_bool_plain(s, table.running.as_ref())? {
                break;
            }
            if start.elapsed() > timeout {
                let _ = self.write_plain(s, table.stop_after.as_ref(), &previous);
                return Err(ScpiError::Timeout);
            }
        }
        let _ = self.write_plain(s, table.stop_after.as_ref(), &previous);
        Ok(())
    }

    fn acquisition_status(&self, s: &mut ScpiSession) -> Result<AcquisitionStatus, ScpiError> {
        if let Some(driver) = self.driver.as_deref() {
            return driver.acquisition_status(s);
        }
        let table = self.table();
        match self.kind() {
            InstrumentKind::Oscilloscope => {
                let running = self.q_bool_plain(s, table.running.as_ref())?;
                Ok(AcquisitionStatus {
                    running,
                    display: if running { "RUN" } else { "STOP" }.into(),
                })
            }
            InstrumentKind::Generator | InstrumentKind::Supply => {
                let nch = self.profile.capabilities.channel_count;
                let mut states = Vec::with_capacity(nch);
                let mut any_on = false;
                for n in 1..=nch {
                    let on = self.q_bool_channel(s, table.channel_enabled.as_ref(), n)?;
                    any_on |= on;
                    states.push(format!("C{n} {}", if on { "OUT" } else { "OFF" }));
                }
                Ok(AcquisitionStatus {
                    running: any_on,
                    display: states.join("  "),
                })
            }
        }
    }

    fn autoset(&self, s: &mut ScpiSession) -> Result<(), ScpiError> {
        if let Some(driver) = self.driver.as_deref() {
            return driver.autoset(s);
        }
        let table = self.table();
        if table.autoset.is_none() {
            return Err(ScpiError::Unsupported(format!(
                "{} has no autoset command in its profile",
                self.name()
            )));
        }
        let cmd = table.autoset.as_ref().and_then(|a| a.plain_write(""));
        let Some(cmd) = cmd.filter(|c| !c.is_empty()) else {
            return Err(ScpiError::Unsupported(format!(
                "{} has no autoset command in its profile",
                self.name()
            )));
        };
        s.write(&cmd)
    }
}

fn user_profile_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("instrument-viewer")
        .join("profiles")
}

fn builtin_profiles() -> Vec<InstrumentProfile> {
    [
        include_str!("../profiles/example.yaml"),
        include_str!("../profiles/tek-mdo3000.yaml"),
        include_str!("../profiles/rigol-dho900.yaml"),
        include_str!("../profiles/siglent-sdg1000x.yaml"),
        include_str!("../profiles/keysight-e36200.yaml"),
        include_str!("../profiles/tek-afg3000.yaml"),
        include_str!("../profiles/siglent-sds1000x-e.yaml"),
    ]
    .into_iter()
    .filter_map(|text| {
        serde_yaml_ng::from_str(text)
            .map_err(|e| {
                eprintln!("[profiles] built-in profile parse failed: {e}");
            })
            .ok()
    })
    .collect()
}

fn load_user_profiles() -> Vec<InstrumentProfile> {
    let Ok(entries) = std::fs::read_dir(user_profile_dir()) else {
        return Vec::new();
    };
    let mut profiles = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if ext != "yaml" && ext != "yml" {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_yaml_ng::from_str::<InstrumentProfile>(&text) {
                Ok(profile) => profiles.push(profile),
                Err(e) => eprintln!("[profiles] {} parse failed: {e}", path.display()),
            },
            Err(e) => eprintln!("[profiles] {} unreadable: {e}", path.display()),
        }
    }
    profiles
}

/// All loaded profiles, user profiles first so they can shadow built-ins.
fn profiles() -> Vec<InstrumentProfile> {
    let mut all = load_user_profiles();
    all.extend(builtin_profiles());
    all
}

/// The first profile whose `idn_matches` substring appears in the `*IDN?`
/// reply, matched case-insensitively.
pub fn matching(idn: &str) -> Option<InstrumentProfile> {
    let upper = idn.to_ascii_uppercase();
    profiles().into_iter().find(|profile| {
        profile
            .idn_matches
            .iter()
            .any(|pattern| upper.contains(&pattern.to_ascii_uppercase()))
    })
}

/// The hand-written driver a profile can delegate quirks to.
fn driver_for(name: &str, idn: &str) -> Option<Box<dyn Backend>> {
    match name {
        "tek" => Some(Box::new(crate::tek::Tek)),
        "rigol" => Some(Box::new(crate::rigol::Rigol::from_idn(idn))),
        "sds" => Some(Box::new(crate::sds::Sds::from_idn(idn))),
        "siglent" => Some(Box::new(crate::siglent::Siglent::from_idn(idn))),
        "afg" => Some(Box::new(crate::afg::Afg::from_idn(idn))),
        "keysight" => Some(Box::new(crate::keysight::KeysightPsu::from_idn(idn))),
        _ => None,
    }
}

/// A `Backend` driven by a data profile, when one matches.
pub fn backend_for(idn: &str) -> Option<Box<dyn Backend>> {
    matching(idn).map(|profile| Box::new(ProfileBackend::new(profile, idn)) as Box<dyn Backend>)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_example_profile_parses() {
        assert!(!builtin_profiles().is_empty());
        let profile = &builtin_profiles()[0];
        assert_eq!(profile.name, "Example Tek-like scope");
        assert_eq!(profile.capabilities.kind, InstrumentKind::Oscilloscope);
        assert!(profile.commands.scale.is_some());
    }

    #[test]
    fn matches_idn_substrings_case_insensitively() {
        let profile = builtin_profiles()
            .into_iter()
            .find(|p| p.idn_matches.iter().any(|m| m == "EXAMPLE,OSC1"))
            .unwrap();
        assert!(profile
            .idn_matches
            .iter()
            .any(|m| "EXAMPLE,OSC1".contains(m.as_str())));
        assert!(matching("Vendor,Example,OSC1,SN,1.0").is_some());
        assert!(matching("TEKTRONIX,MDO3024,SN,1.0").is_some());
    }

    #[test]
    fn profile_backend_reports_its_capabilities() {
        let profile = builtin_profiles().into_iter().next().unwrap();
        let backend = ProfileBackend::new(profile, "EXAMPLE,OSC1");
        assert_eq!(backend.name(), "Example Tek-like scope");
        assert_eq!(backend.capabilities().channel_count, 2);
        assert_eq!(backend.kind(), InstrumentKind::Oscilloscope);
        assert_eq!(backend.waveform_format(), Some(WaveformFormat::TEK));
    }

    #[test]
    fn all_supported_devices_have_profiles() {
        let cases = [
            ("TEKTRONIX,MDO3024,SN,1.0", "Tektronix"),
            ("RIGOL TECHNOLOGIES,DHO924S,SN,00.01.05", "Rigol DHO924S"),
            ("Siglent Technologies,SDG1032X,SN,1.0", "Siglent SDG1032X"),
            (
                "Keysight Technologies,E36233A,SN,1.1.1-1.0.3-1.01",
                "Keysight E36233A",
            ),
            ("TEKTRONIX,AFG3051C,SN,1.0", "Tektronix AFG3051C"),
            (
                "Siglent Technologies,SDS1104X-E,SN,7.6.1.15",
                "Siglent SDS1104X-E",
            ),
        ];
        for (idn, want) in cases {
            let backend = crate::backend::from_idn(idn);
            assert_eq!(backend.name(), want, "backend for {idn}");
        }
    }
}
