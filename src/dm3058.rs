//! Rigol DM3058/DM3058E digital multimeter.
//!
//! A single-function bench DMM: the function is selected with `:FUNCtion:*`
//! and read back as a short enum (`DCV`, `ACV`, …); the measurement itself is
//! a one-line scientific-notation reply to `:MEASure:<function>?`. The app
//! presents it as one channel whose reading is the fetched value and whose
//! "function" is the wave type.

use std::time::Duration;

use crate::backend::{
    AcquisitionStatus, Backend, InstrumentCapabilities, InstrumentKind, ValueChoice,
};
use crate::config::{
    AcquisitionConfig, ChannelConfig, HorizontalConfig, InstrumentConfig, TriggerConfig,
};
use crate::scpi::{ScpiError, ScpiSession};
use crate::waveform::{ChannelTrace, WaveformError};

/// Functions the DM3058 offers, in the app's internal spelling.
pub const FUNCTIONS: &[&str] = &[
    "DCV", "ACV", "DCI", "ACI", "R2W", "R4W", "FREQ", "PERI", "CAP", "CONT", "DIOD",
];

/// Rigol DM3058/DM3058E.
pub struct Dm3058 {
    name: String,
}

impl Dm3058 {
    pub fn from_idn(idn: &str) -> Self {
        let model = idn
            .split(',')
            .nth(1)
            .unwrap_or("")
            .trim()
            .to_ascii_uppercase();
        Self {
            name: if model.is_empty() {
                "Rigol DM3058".to_string()
            } else {
                format!("Rigol {model}")
            },
        }
    }

    fn current_function(&self, s: &mut ScpiSession) -> Result<String, ScpiError> {
        Ok(crate::scpi::parse_character(&s.query(":FUNC?")?))
    }

    fn reading(&self, s: &mut ScpiSession) -> Result<(String, f64), ScpiError> {
        let function = self.current_function(s)?;
        let command = measure_command(&function)
            .ok_or_else(|| ScpiError::Unsupported(format!("{function} cannot be measured")))?;
        let value = crate::scpi::parse_f64(&s.query(command)?)?;
        Ok((function, value))
    }
}

fn function_command(function: &str) -> Option<&'static str> {
    Some(match function {
        "DCV" => ":FUNC:VOLT:DC",
        "ACV" => ":FUNC:VOLT:AC",
        "DCI" => ":FUNC:CURR:DC",
        "ACI" => ":FUNC:CURR:AC",
        "R2W" => ":FUNC:RES",
        "R4W" => ":FUNC:FRES",
        "FREQ" => ":FUNC:FREQ",
        "PERI" => ":FUNC:PER",
        "CAP" => ":FUNC:CAP",
        "CONT" => ":FUNC:CONT",
        "DIOD" => ":FUNC:DIOD",
        _ => return None,
    })
}

fn measure_command(function: &str) -> Option<&'static str> {
    Some(match function {
        "DCV" => ":MEAS:VOLT:DC?",
        "ACV" => ":MEAS:VOLT:AC?",
        "DCI" => ":MEAS:CURR:DC?",
        "ACI" => ":MEAS:CURR:AC?",
        "R2W" => ":MEAS:RES?",
        "R4W" => ":MEAS:FRES?",
        "FREQ" => ":MEAS:FREQ?",
        "PERI" => ":MEAS:PER?",
        "CAP" => ":MEAS:CAP?",
        "CONT" => ":MEAS:CONT?",
        "DIOD" => ":MEAS:DIOD?",
        _ => return None,
    })
}

fn unit_for(function: &str) -> &'static str {
    match function {
        "DCI" | "ACI" => "A",
        "R2W" | "R4W" | "CONT" => "Ω",
        "FREQ" => "Hz",
        "PERI" => "s",
        "CAP" => "F",
        _ => "V",
    }
}

impl Backend for Dm3058 {
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
                "Single-function 5½-digit DMM; the function is the \"wave type\".".into(),
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
        s.write(command)
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
        let function = self.current_function(s)?;
        Ok(AcquisitionStatus {
            running: true,
            display: function,
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
            Dm3058::from_idn("RIGOL TECHNOLOGIES,DM3058,DM3A123456789,00.01.00").name(),
            "Rigol DM3058"
        );
        assert_eq!(Dm3058::from_idn("").name(), "Rigol DM3058");
    }

    #[test]
    fn maps_functions_and_units() {
        assert_eq!(function_command("DCV"), Some(":FUNC:VOLT:DC"));
        assert_eq!(measure_command("DCV"), Some(":MEAS:VOLT:DC?"));
        assert_eq!(unit_for("DCV"), "V");
        assert_eq!(function_command("R2W"), Some(":FUNC:RES"));
        assert_eq!(measure_command("R4W"), Some(":MEAS:FRES?"));
        assert_eq!(unit_for("DCI"), "A");
        assert_eq!(unit_for("FREQ"), "Hz");
        assert_eq!(unit_for("CAP"), "F");
        assert_eq!(function_command("NOPE"), None);
    }
}
