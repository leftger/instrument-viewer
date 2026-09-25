//! Hantek HDM3000 benchtop digital multimeter.
//!
//! Agilent 34401-style command set: the function is selected with
//! `FUNC "<function>"` and read back as an abbreviated quoted string
//! (`"VOLT"`, `"CURR:AC"`, …); `READ?` performs one measurement and returns
//! the value in scientific notation. The app presents the selected function as
//! the "wave type" and the reading as the channel value.

use std::time::Duration;

use crate::backend::{
    AcquisitionStatus, Backend, InstrumentCapabilities, InstrumentKind, ValueChoice,
};
use crate::config::{
    AcquisitionConfig, ChannelConfig, HorizontalConfig, InstrumentConfig, TriggerConfig,
};
use crate::scpi::{ScpiError, ScpiSession};
use crate::waveform::{ChannelTrace, WaveformError};

/// Functions the HDM3000 offers, in the app's internal spelling.
pub const FUNCTIONS: &[&str] = &[
    "DCV", "ACV", "DCI", "ACI", "R2W", "R4W", "FREQ", "PERI", "CAP", "CONT", "DIOD", "TEMP",
];

/// Hantek HDM3000.
pub struct Hdm3000 {
    name: String,
}

impl Hdm3000 {
    pub fn from_idn(idn: &str) -> Self {
        let model = idn
            .split(',')
            .nth(1)
            .unwrap_or("")
            .trim()
            .to_ascii_uppercase();
        Self {
            name: if model.is_empty() {
                "Hantek HDM3000".to_string()
            } else {
                format!("Hantek {model}")
            },
        }
    }

    fn current_function(&self, s: &mut ScpiSession) -> Result<String, ScpiError> {
        Ok(function_from_reply(&s.query("FUNC?")?))
    }

    fn reading(&self, s: &mut ScpiSession) -> Result<(String, f64), ScpiError> {
        let function = self.current_function(s)?;
        // `READ?` triggers one measurement; a scan would answer with a list.
        let reply = s.query("READ?")?;
        let first = reply.split(',').next().unwrap_or(&reply);
        let value = crate::scpi::parse_f64(first)?;
        Ok((function, value))
    }
}

/// The `FUNC "<function>"` spelling for an internal function name.
fn function_command(function: &str) -> Option<&'static str> {
    Some(match function {
        "DCV" => "VOLT:DC",
        "ACV" => "VOLT:AC",
        "DCI" => "CURR:DC",
        "ACI" => "CURR:AC",
        "R2W" => "RES",
        "R4W" => "FRES",
        "FREQ" => "FREQ",
        "PERI" => "PER",
        "CAP" => "CAP",
        "CONT" => "CONT",
        "DIOD" => "DIOD",
        "TEMP" => "TEMP",
        _ => return None,
    })
}

/// Normalize the abbreviated quoted reply from `FUNC?` into an internal name.
fn function_from_reply(reply: &str) -> String {
    // The reply is quoted, e.g. `"CURR:AC"`; parse_character strips the quotes.
    let text = crate::scpi::parse_character(reply);
    match text.as_str() {
        "VOLT" | "VOLT:DC" | "VOLTAGE" => "DCV".into(),
        "VOLT:AC" => "ACV".into(),
        "CURR" | "CURR:DC" => "DCI".into(),
        "CURR:AC" => "ACI".into(),
        "RES" => "R2W".into(),
        "FRES" => "R4W".into(),
        "FREQ" => "FREQ".into(),
        "PER" => "PERI".into(),
        "CAP" => "CAP".into(),
        "CONT" => "CONT".into(),
        "DIOD" => "DIOD".into(),
        "TEMP" => "TEMP".into(),
        other => other.to_string(),
    }
}

fn unit_for(function: &str) -> &'static str {
    match function {
        "DCI" | "ACI" => "A",
        "R2W" | "R4W" | "CONT" => "Ω",
        "FREQ" => "Hz",
        "PERI" => "s",
        "CAP" => "F",
        "TEMP" => "°C",
        _ => "V",
    }
}

impl Backend for Hdm3000 {
    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> InstrumentKind {
        InstrumentKind::Multimeter
    }

    fn capabilities(&self) -> InstrumentCapabilities {
        InstrumentCapabilities {
            channel_couplings: vec!["DC".into()],
            terminations: vec![ValueChoice::new("—", 1e6)],
            termination_writable: false,
            bandwidths: vec![ValueChoice::new("N/A", 0.0)],
            record_lengths: vec![1],
            trigger_modes: vec!["NONE".into()],
            trigger_slopes: vec!["—".into()],
            trigger_couplings: vec!["—".into()],
            acquisition_modes: vec!["MEASURE".into()],
            stop_after: vec!["RUNSTOP".into()],
            channel_hint: Some(
                "Hantek HDM3000 bench DMM; the function is the \"wave type\".".into(),
            ),
            horizontal_hint: None,
            acquisition_hint: Some("Fetch takes one reading per press.".into()),
            kind: InstrumentKind::Multimeter,
            channel_count: 1,
            wave_types: FUNCTIONS.iter().map(|s| (*s).to_string()).collect(),
            output_pairs: Vec::new(),
        }
    }

    fn read_config(&self, s: &mut ScpiSession) -> Result<InstrumentConfig, ScpiError> {
        let function = self.current_function(s)?;
        // Measuring on every read is slow but keeps the panel honest; a failed
        // read must not break the rest of the synchronize.
        let (function, value) = self.reading(s).unwrap_or((function, f64::NAN));
        Ok(InstrumentConfig {
            channels: vec![ChannelConfig {
                enabled: true,
                scale: value,
                position: 0.0,
                offset: 0.0,
                coupling: "DC".into(),
                termination_ohms: 1e6,
                bandwidth_hz: 0.0,
                probe_gain: 1.0,
                probe_type: unit_for(&function).into(),
                wave_type: function,
                frequency_hz: 0.0,
            }],
            horizontal: HorizontalConfig {
                scale: 1.0,
                position: 50.0,
                record_length: 1,
            },
            trigger: TriggerConfig {
                mode: "NONE".into(),
                source: "CH1".into(),
                slope: "—".into(),
                coupling: "—".into(),
                level: 0.0,
            },
            acquisition: AcquisitionConfig {
                mode: "MEASURE".into(),
                stop_after: "RUNSTOP".into(),
                running: true,
            },
            output_pair: String::new(),
        })
    }

    fn apply_channel(
        &self,
        s: &mut ScpiSession,
        _n: usize,
        ch: &ChannelConfig,
    ) -> Result<(), ScpiError> {
        let command = function_command(&ch.wave_type)
            .ok_or_else(|| ScpiError::Unsupported(format!("function {}", ch.wave_type)))?;
        // The instrument wants the function as a quoted string.
        s.write(&format!("FUNC \"{command}\""))
    }

    fn apply_horizontal(
        &self,
        _s: &mut ScpiSession,
        _h: &HorizontalConfig,
    ) -> Result<(), ScpiError> {
        Ok(())
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
        let (function, value) = self.reading(s)?;
        Ok(ChannelTrace {
            channel: format!("DMM {function}"),
            x_unit: "s".into(),
            y_unit: unit_for(&function).into(),
            points: vec![[0.0, value]],
        })
    }

    fn wait_sequence(&self, _s: &mut ScpiSession, _timeout: Duration) -> Result<(), ScpiError> {
        Ok(())
    }

    fn acquisition_status(&self, s: &mut ScpiSession) -> Result<AcquisitionStatus, ScpiError> {
        Ok(AcquisitionStatus {
            running: true,
            display: self.current_function(s)?,
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
            Hdm3000::from_idn("Hantek,HDM3000,SN,1.02").name(),
            "Hantek HDM3000"
        );
        assert_eq!(Hdm3000::from_idn("").name(), "Hantek HDM3000");
    }

    #[test]
    fn maps_functions_units_and_replies() {
        assert_eq!(function_command("DCV"), Some("VOLT:DC"));
        assert_eq!(function_command("R4W"), Some("FRES"));
        assert_eq!(function_command("TEMP"), Some("TEMP"));
        assert_eq!(function_command("NOPE"), None);
        assert_eq!(function_from_reply("\"VOLT\""), "DCV");
        assert_eq!(function_from_reply("\"VOLT:AC\""), "ACV");
        assert_eq!(function_from_reply("\"CURR:AC\""), "ACI");
        assert_eq!(function_from_reply("\"CONT\""), "CONT");
        assert_eq!(unit_for("ACI"), "A");
        assert_eq!(unit_for("R2W"), "Ω");
        assert_eq!(unit_for("TEMP"), "°C");
    }

    #[test]
    fn quoted_function_command_is_valid_scpi() {
        assert!(ScpiSession::validate_program("FUNC \"VOLT:DC\"").is_ok());
    }
}
