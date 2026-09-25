//! Hantek DAQ4000A data acquisition / scanning multimeter.
//!
//! Agilent 34970A-style command set. The instrument scans a channel list
//! (`ROUTe:SCAN (@101,102)`), each channel carrying a function selected with
//! `FUNC "<function>",(@<ch_list>)`. `READ?` performs one scan and answers with
//! one comma-separated reading per scanned channel, so a single fetch becomes
//! one trace per channel in the app's meter view.

use std::time::Duration;

use crate::backend::{
    AcquisitionStatus, Backend, InstrumentCapabilities, InstrumentKind, ValueChoice,
};
use crate::config::{
    AcquisitionConfig, ChannelConfig, HorizontalConfig, InstrumentConfig, TriggerConfig,
};
use crate::scpi::{ScpiError, ScpiSession};
use crate::waveform::{ChannelTrace, WaveformError};

/// Functions this driver exposes, in the app's internal spelling. Strain and
/// temperature-probe variants are left for a later pass: they need bridge and
/// probe configuration to be meaningful.
pub const FUNCTIONS: &[&str] = &[
    "DCV", "ACV", "DCI", "ACI", "R2W", "R4W", "FREQ", "PERI", "CAP", "DIOD", "TEMP",
];

/// Channel list used when the instrument reports no scan list.
const DEFAULT_SCAN: &str = "(@101)";

/// Hantek DAQ4000A.
pub struct Daq4000a {
    name: String,
}

impl Daq4000a {
    pub fn from_idn(idn: &str) -> Self {
        let model = idn
            .split(',')
            .nth(1)
            .unwrap_or("")
            .trim()
            .to_ascii_uppercase();
        Self {
            name: if model.is_empty() {
                "Hantek DAQ4000A".to_string()
            } else {
                format!("Hantek {model}")
            },
        }
    }

    /// The scan list as channel numbers, e.g. `[101, 102]`.
    fn scan_channels(&self, s: &mut ScpiSession) -> Result<Vec<u32>, ScpiError> {
        let list = match s.query_binary_block("ROUTe:SCAN?") {
            Ok(block) => match std::str::from_utf8(&block) {
                Ok(text) => parse_scan_list(text),
                Err(_) => Vec::new(),
            },
            // A channel list query is worth a fallback: an empty or rejected
            // scan list should not stop a single-channel reading.
            Err(_) => Vec::new(),
        };
        if list.is_empty() {
            Ok(parse_scan_list(DEFAULT_SCAN))
        } else {
            Ok(list)
        }
    }

    fn current_function(&self, s: &mut ScpiSession, channel: u32) -> Result<String, ScpiError> {
        let reply = s.query(&format!("FUNC? (@{channel})"))?;
        Ok(function_from_reply(&reply))
    }

    /// One scan, as (function, [(channel, value)]).
    fn scan(&self, s: &mut ScpiSession) -> Result<(String, Vec<(u32, f64)>), ScpiError> {
        let channels = self.scan_channels(s)?;
        let function = self
            .current_function(s, channels.first().copied().unwrap_or(101))
            .unwrap_or_else(|_| "DCV".into());
        let reply = s.query("READ?")?;
        let values: Vec<f64> = reply
            .split(',')
            .filter_map(|part| crate::scpi::parse_f64(part.trim()).ok())
            .collect();
        let readings = channels
            .iter()
            .enumerate()
            .map(|(i, channel)| (*channel, values.get(i).copied().unwrap_or(f64::NAN)))
            .collect();
        Ok((function, readings))
    }
}

/// The `FUNC "<function>",(@<ch_list>)` spelling for an internal function name.
fn function_command(function: &str) -> Option<&'static str> {
    Some(match function {
        "DCV" => "VOLT",
        "ACV" => "VOLT:AC",
        "DCI" => "CURR",
        "ACI" => "CURR:AC",
        "R2W" => "RES",
        "R4W" => "FRES",
        "FREQ" => "FREQ",
        "PERI" => "PER",
        "CAP" => "CAP",
        "DIOD" => "DIOD",
        "TEMP" => "TEMP:TC",
        _ => return None,
    })
}

/// Normalize the function reply (`"VOLT"`, `"TEMP:TC"`, …) into an internal name.
fn function_from_reply(reply: &str) -> String {
    // The reply can be quoted and carry the channel list after a comma:
    // `"VOLT",(@101)`. Take the text inside the first pair of quotes.
    let token = match reply.find('"') {
        Some(start) => {
            let rest = &reply[start + 1..];
            rest.split('"').next().unwrap_or(rest)
        }
        None => reply.split(',').next().unwrap_or(reply),
    };
    match token.trim().to_ascii_uppercase().as_str() {
        "VOLT" | "VOLT:DC" | "VOLTAGE" => "DCV".into(),
        "VOLT:AC" => "ACV".into(),
        "CURR" | "CURR:DC" => "DCI".into(),
        "CURR:AC" => "ACI".into(),
        "RES" => "R2W".into(),
        "FRES" => "R4W".into(),
        "FREQ" => "FREQ".into(),
        "PER" => "PERI".into(),
        "CAP" => "CAP".into(),
        "DIOD" => "DIOD".into(),
        other if other.starts_with("TEMP") => "TEMP".into(),
        other => other.to_string(),
    }
}

/// Extract the channel numbers from `(@101,102)` or `101,102`.
fn parse_scan_list(text: &str) -> Vec<u32> {
    let inner = text
        .trim()
        .trim_matches(|c: char| c == '(' || c == ')' || c == '@' || c.is_whitespace());
    inner
        .split(',')
        .filter_map(|part| part.trim().parse::<u32>().ok())
        .take(1024)
        .collect()
}

fn unit_for(function: &str) -> &'static str {
    match function {
        "DCI" | "ACI" => "A",
        "R2W" | "R4W" => "Ω",
        "FREQ" => "Hz",
        "PERI" => "s",
        "CAP" => "F",
        "TEMP" => "°C",
        _ => "V",
    }
}

impl Backend for Daq4000a {
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
            acquisition_modes: vec!["SCAN".into()],
            stop_after: vec!["RUNSTOP".into()],
            channel_hint: Some(
                "Scans the instrument's channel list; one reading per scanned channel.".into(),
            ),
            horizontal_hint: None,
            acquisition_hint: Some("Fetch runs one scan of the channel list.".into()),
            kind: InstrumentKind::Multimeter,
            channel_count: 1,
            wave_types: FUNCTIONS.iter().map(|s| (*s).to_string()).collect(),
            output_pairs: Vec::new(),
        }
    }

    fn read_config(&self, s: &mut ScpiSession) -> Result<InstrumentConfig, ScpiError> {
        let (function, readings) = self.scan(s).unwrap_or_else(|_| ("DCV".into(), Vec::new()));
        let unit = unit_for(&function);
        let channels = if readings.is_empty() {
            vec![ChannelConfig {
                enabled: true,
                scale: f64::NAN,
                position: 0.0,
                offset: 0.0,
                coupling: "DC".into(),
                termination_ohms: 1e6,
                bandwidth_hz: 0.0,
                probe_gain: 1.0,
                probe_type: unit.into(),
                wave_type: function.clone(),
                frequency_hz: 0.0,
            }]
        } else {
            readings
                .iter()
                .map(|(_, value)| ChannelConfig {
                    enabled: true,
                    scale: *value,
                    position: 0.0,
                    offset: 0.0,
                    coupling: "DC".into(),
                    termination_ohms: 1e6,
                    bandwidth_hz: 0.0,
                    probe_gain: 1.0,
                    probe_type: unit.into(),
                    wave_type: function.clone(),
                    frequency_hz: 0.0,
                })
                .collect()
        };
        Ok(InstrumentConfig {
            channels,
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
                mode: "SCAN".into(),
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
        // One function applies to the whole scan list in this app.
        let channels = self.scan_channels(s)?;
        let list = channels
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(",");
        s.write(&format!("FUNC \"{command}\",(@{list})"))
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

    fn fetch_channel(&self, s: &mut ScpiSession, ch: &str) -> Result<ChannelTrace, WaveformError> {
        // A scan is all-or-nothing; ignore the requested name and return the
        // whole scan as one trace.
        let (function, readings) = self.scan(s)?;
        let first = readings
            .first()
            .map(|(_, value)| *value)
            .unwrap_or(f64::NAN);
        Ok(ChannelTrace {
            channel: ch.to_string(),
            x_unit: "s".into(),
            y_unit: unit_for(&function).into(),
            points: vec![[0.0, first]],
        })
    }

    fn fetch_channels(
        &self,
        s: &mut ScpiSession,
        _channels: &[String],
    ) -> Result<Vec<ChannelTrace>, WaveformError> {
        let (function, readings) = self.scan(s)?;
        if readings.is_empty() {
            return Err(WaveformError::Parse("scan returned no readings".into()));
        }
        let unit = unit_for(&function);
        Ok(readings
            .into_iter()
            .map(|(channel, value)| ChannelTrace {
                channel: format!("CH{channel}"),
                x_unit: "s".into(),
                y_unit: unit.into(),
                points: vec![[0.0, value]],
            })
            .collect())
    }

    fn wait_sequence(&self, _s: &mut ScpiSession, _timeout: Duration) -> Result<(), ScpiError> {
        Ok(())
    }

    fn acquisition_status(&self, s: &mut ScpiSession) -> Result<AcquisitionStatus, ScpiError> {
        let size = self.scan_channels(s).map(|c| c.len()).unwrap_or(1);
        let function = self.current_function(s, 101).unwrap_or_else(|_| "—".into());
        Ok(AcquisitionStatus {
            running: true,
            display: format!("{function} · {size} ch"),
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
            Daq4000a::from_idn("Hantek,DAQ4000A,SN,1.01").name(),
            "Hantek DAQ4000A"
        );
        assert_eq!(Daq4000a::from_idn("").name(), "Hantek DAQ4000A");
    }

    #[test]
    fn parses_scan_lists() {
        assert_eq!(parse_scan_list("(@101,102)"), vec![101, 102]);
        assert_eq!(parse_scan_list("(@101)"), vec![101]);
        assert_eq!(parse_scan_list("101,103"), vec![101, 103]);
        assert_eq!(parse_scan_list("garbage"), Vec::<u32>::new());
    }

    #[test]
    fn maps_functions_and_replies() {
        assert_eq!(function_command("DCV"), Some("VOLT"));
        assert_eq!(function_command("ACI"), Some("CURR:AC"));
        assert_eq!(function_command("TEMP"), Some("TEMP:TC"));
        assert_eq!(function_from_reply("\"VOLT\""), "DCV");
        assert_eq!(function_from_reply("\"TEMP:TC\""), "TEMP");
        assert_eq!(function_from_reply("\"VOLT\",(@101)"), "DCV");
        assert_eq!(unit_for("R4W"), "Ω");
    }

    #[test]
    fn scan_function_command_is_valid_scpi() {
        assert!(ScpiSession::validate_program("FUNC \"VOLT\",(@101,102)").is_ok());
    }
}
