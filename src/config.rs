use crate::backend::{Backend, InstrumentCapabilities, InstrumentKind};
use crate::scpi::{parse_bool, parse_character, parse_count, parse_f64, ScpiError, ScpiSession};

#[derive(Clone, Debug)]
pub struct ChannelConfig {
    pub enabled: bool,
    pub scale: f64,
    pub position: f64,
    pub offset: f64,
    pub coupling: String,
    pub termination_ohms: f64,
    pub bandwidth_hz: f64,
    /// Probe output/input transfer ratio: 0.1 means a 10x probe.
    pub probe_gain: f64,
    pub probe_type: String,
    /// Generator wave type (`SINE`, `SQUARE`, …). Empty on scopes and supplies.
    pub wave_type: String,
    /// Generator frequency in Hz. Zero on oscilloscopes.
    /// Power-supply current limit is stored in `offset`; voltage setpoint in `scale`.
    pub frequency_hz: f64,
}

#[derive(Clone, Debug)]
pub struct HorizontalConfig {
    pub scale: f64,
    pub position: f64,
    pub record_length: u64,
}

#[derive(Clone, Debug)]
pub struct TriggerConfig {
    pub mode: String,
    pub source: String,
    pub slope: String,
    pub coupling: String,
    pub level: f64,
}

#[derive(Clone, Debug)]
pub struct AcquisitionConfig {
    pub mode: String,
    pub stop_after: String,
    pub running: bool,
}

#[derive(Clone, Debug)]
pub struct InstrumentConfig {
    pub channels: [ChannelConfig; 4],
    pub horizontal: HorizontalConfig,
    pub trigger: TriggerConfig,
    pub acquisition: AcquisitionConfig,
}

#[derive(Clone, Debug)]
pub enum ConfigSection {
    Channel(usize, ChannelConfig),
    Horizontal(HorizontalConfig),
    Trigger(TriggerConfig),
    Acquisition(AcquisitionConfig),
}

/// Preserve instrument-reported values even when a model or firmware exposes a
/// choice not present in our static profile. This keeps capability validation
/// from rejecting an unchanged field while still constraining new GUI choices.
pub fn include_current_values(
    capabilities: &mut InstrumentCapabilities,
    config: &InstrumentConfig,
) {
    for channel in &config.channels {
        include_string(&mut capabilities.channel_couplings, &channel.coupling);
        if !channel.wave_type.is_empty() {
            include_string(&mut capabilities.wave_types, &channel.wave_type);
        }
        include_number(
            &mut capabilities.terminations,
            "Instrument value",
            channel.termination_ohms,
        );
        include_number(
            &mut capabilities.bandwidths,
            &format!("{} MHz", channel.bandwidth_hz / 1e6),
            channel.bandwidth_hz,
        );
    }
    if !capabilities
        .record_lengths
        .contains(&config.horizontal.record_length)
    {
        capabilities
            .record_lengths
            .push(config.horizontal.record_length);
        capabilities.record_lengths.sort_unstable();
    }
    include_string(&mut capabilities.trigger_modes, &config.trigger.mode);
    include_string(&mut capabilities.trigger_slopes, &config.trigger.slope);
    include_string(
        &mut capabilities.trigger_couplings,
        &config.trigger.coupling,
    );
    include_string(
        &mut capabilities.acquisition_modes,
        &config.acquisition.mode,
    );
    include_string(&mut capabilities.stop_after, &config.acquisition.stop_after);
}

fn include_string(choices: &mut Vec<String>, value: &str) {
    if !choices
        .iter()
        .any(|choice| choice.eq_ignore_ascii_case(value))
    {
        choices.push(value.to_string());
    }
}

fn include_number(choices: &mut Vec<crate::backend::ValueChoice>, label: &str, value: f64) {
    if !choices.iter().any(|choice| {
        let scale = value.abs().max(choice.value.abs()).max(1.0);
        (value - choice.value).abs() <= scale * 1e-9
    }) {
        choices.push(crate::backend::ValueChoice::new(label, value));
    }
}

pub fn validate_section(
    section: &ConfigSection,
    capabilities: &InstrumentCapabilities,
) -> Result<(), ScpiError> {
    match section {
        ConfigSection::Channel(_, channel) => {
            require_string(
                "channel coupling",
                &channel.coupling,
                &capabilities.channel_couplings,
            )?;
            require_number(
                "input termination",
                channel.termination_ohms,
                &capabilities
                    .terminations
                    .iter()
                    .map(|choice| choice.value)
                    .collect::<Vec<_>>(),
            )?;
            require_number(
                "bandwidth",
                channel.bandwidth_hz,
                &capabilities
                    .bandwidths
                    .iter()
                    .map(|choice| choice.value)
                    .collect::<Vec<_>>(),
            )?;
            if !capabilities.wave_types.is_empty() {
                require_string("wave type", &channel.wave_type, &capabilities.wave_types)?;
            }
        }
        ConfigSection::Horizontal(horizontal) => {
            if !capabilities
                .record_lengths
                .contains(&horizontal.record_length)
            {
                return Err(unsupported(
                    "record length",
                    horizontal.record_length,
                    capabilities
                        .record_lengths
                        .iter()
                        .map(u64::to_string)
                        .collect(),
                ));
            }
        }
        ConfigSection::Trigger(trigger) => {
            require_string("trigger mode", &trigger.mode, &capabilities.trigger_modes)?;
            require_string(
                "trigger slope",
                &trigger.slope,
                &capabilities.trigger_slopes,
            )?;
            require_string(
                "trigger coupling",
                &trigger.coupling,
                &capabilities.trigger_couplings,
            )?;
        }
        ConfigSection::Acquisition(acquisition) => {
            require_string(
                "acquisition mode",
                &acquisition.mode,
                &capabilities.acquisition_modes,
            )?;
            require_string(
                "stop-after mode",
                &acquisition.stop_after,
                &capabilities.stop_after,
            )?;
        }
    }
    Ok(())
}

fn require_string(name: &str, value: &str, choices: &[String]) -> Result<(), ScpiError> {
    if choices
        .iter()
        .any(|choice| choice.eq_ignore_ascii_case(value))
    {
        Ok(())
    } else {
        Err(unsupported(name, value, choices.to_vec()))
    }
}

fn require_number(name: &str, value: f64, choices: &[f64]) -> Result<(), ScpiError> {
    if choices.iter().any(|choice| {
        let scale = value.abs().max(choice.abs()).max(1.0);
        (value - choice).abs() <= scale * 1e-9
    }) {
        Ok(())
    } else {
        Err(unsupported(
            name,
            value,
            choices.iter().map(|choice| choice.to_string()).collect(),
        ))
    }
}

fn unsupported(name: &str, value: impl std::fmt::Display, choices: Vec<String>) -> ScpiError {
    ScpiError::Unsupported(format!(
        "unsupported {name} {value}; choose {}",
        choices.join(", ")
    ))
}

pub fn read_config(
    session: &mut ScpiSession,
    backend: &dyn Backend,
) -> Result<InstrumentConfig, ScpiError> {
    if backend.kind() == InstrumentKind::Generator {
        return crate::siglent::read_config(session, backend);
    }
    if backend.kind() == InstrumentKind::Supply {
        return crate::keysight::read_config(session, backend);
    }
    let channels = [
        read_channel(session, backend, 1)?,
        read_channel(session, backend, 2)?,
        read_channel(session, backend, 3)?,
        read_channel(session, backend, 4)?,
    ];

    let horizontal = HorizontalConfig {
        scale: query_f64(session, "HORIZONTAL:SCALE?")?,
        position: query_f64(session, "HORIZONTAL:POSITION?")?,
        record_length: query_u64(session, "HORIZONTAL:RECORDLENGTH?")?,
    };

    let source = clean_enum(&session.query("TRIGGER:A:EDGE:SOURCE?")?);
    let trigger = TriggerConfig {
        mode: clean_enum(&session.query("TRIGGER:A:MODE?")?),
        source: source.clone(),
        slope: clean_enum(&session.query("TRIGGER:A:EDGE:SLOPE?")?),
        coupling: clean_enum(&session.query("TRIGGER:A:EDGE:COUPLING?")?),
        level: backend.trigger_level(session, &source)?,
    };

    let acquisition = AcquisitionConfig {
        mode: clean_enum(&session.query("ACQUIRE:MODE?")?),
        stop_after: clean_enum(&session.query("ACQUIRE:STOPAFTER?")?),
        running: query_bool(session, "ACQUIRE:STATE?")?,
    };

    Ok(InstrumentConfig {
        channels,
        horizontal,
        trigger,
        acquisition,
    })
}

fn read_channel(
    session: &mut ScpiSession,
    backend: &dyn Backend,
    number: usize,
) -> Result<ChannelConfig, ScpiError> {
    Ok(ChannelConfig {
        enabled: backend.channel_enabled(session, number)?,
        scale: query_f64(session, &format!("CH{number}:SCALE?"))?,
        position: query_f64(session, &format!("CH{number}:POSITION?"))?,
        offset: query_f64(session, &format!("CH{number}:OFFSET?"))?,
        coupling: clean_enum(&session.query(&format!("CH{number}:COUPLING?"))?),
        termination_ohms: backend.termination_ohms(session, number)?,
        bandwidth_hz: backend.bandwidth_hz(session, number)?,
        probe_gain: query_f64(session, &format!("CH{number}:PROBE:GAIN?"))?,
        probe_type: backend.probe_type(session, number)?,
        wave_type: String::new(),
        frequency_hz: 0.0,
    })
}

pub fn apply_section(
    session: &mut ScpiSession,
    backend: &dyn Backend,
    section: &ConfigSection,
) -> Result<(), ScpiError> {
    match section {
        ConfigSection::Channel(index, ch) if backend.kind() == InstrumentKind::Generator => {
            crate::siglent::apply_channel(session, index + 1, ch)?;
        }
        ConfigSection::Channel(index, ch) if backend.kind() == InstrumentKind::Supply => {
            crate::keysight::apply_channel(session, backend, index + 1, ch)?;
        }
        ConfigSection::Channel(index, ch) => {
            let n = index + 1;
            // Probe gain changes the engineering units of scale/offset, so set it first.
            session.write(&format!("CH{n}:PROBE:GAIN {}", ch.probe_gain))?;
            backend.set_channel_enabled(session, n, ch.enabled)?;
            session.write(&format!("CH{n}:COUPLING {}", ch.coupling))?;
            backend.set_termination_ohms(session, n, ch.termination_ohms)?;
            backend.set_bandwidth_hz(session, n, ch.bandwidth_hz)?;
            session.write(&format!("CH{n}:SCALE {}", ch.scale))?;
            session.write(&format!("CH{n}:POSITION {}", ch.position))?;
            session.write(&format!("CH{n}:OFFSET {}", ch.offset))?;
        }
        ConfigSection::Horizontal(_) | ConfigSection::Trigger(_) if !backend.kind().is_scope() => {}
        ConfigSection::Horizontal(h) => {
            session.write(&format!("HORIZONTAL:RECORDLENGTH {}", h.record_length))?;
            session.write(&format!("HORIZONTAL:SCALE {}", h.scale))?;
            session.write(&format!("HORIZONTAL:POSITION {}", h.position))?;
        }
        ConfigSection::Trigger(t) => {
            session.write("TRIGGER:A:TYPE EDGE")?;
            session.write(&format!("TRIGGER:A:MODE {}", t.mode))?;
            session.write(&format!("TRIGGER:A:EDGE:SOURCE {}", t.source))?;
            session.write(&format!("TRIGGER:A:EDGE:SLOPE {}", t.slope))?;
            session.write(&format!("TRIGGER:A:EDGE:COUPLING {}", t.coupling))?;
            backend.set_trigger_level(session, &t.source, t.level)?;
        }
        ConfigSection::Acquisition(a) => {
            backend.apply_acquisition(session, &a.mode, &a.stop_after, a.running)?;
        }
    }
    // Wait until all preceding setters have been processed before reporting success.
    let _ = session.query("*OPC?")?;
    Ok(())
}

pub fn query_f64(session: &mut ScpiSession, command: &str) -> Result<f64, ScpiError> {
    let response = session.query(command)?;
    parse_f64(&response).map_err(|_| ScpiError::Parse(format!("{command} returned {response:?}")))
}

fn query_u64(session: &mut ScpiSession, command: &str) -> Result<u64, ScpiError> {
    let response = session.query(command)?;
    parse_count(&response)
        .ok_or_else(|| ScpiError::Parse(format!("{command} returned {response:?}")))
}

pub fn query_bool(session: &mut ScpiSession, command: &str) -> Result<bool, ScpiError> {
    let response = session.query(command)?;
    parse_bool(&response).or_else(|_| match clean_enum(&response).as_str() {
        "1" | "ON" | "RUN" => Ok(true),
        "0" | "OFF" | "STOP" => Ok(false),
        _ => Err(ScpiError::Parse(format!("{command} returned {response:?}"))),
    })
}

fn clean_enum(value: &str) -> String {
    let value = parse_character(value);
    match value.as_str() {
        "AUT" => "AUTO",
        "NORM" => "NORMAL",
        "RIS" => "RISE",
        "FAL" => "FALL",
        "EIT" => "EITHER",
        "SAM" => "SAMPLE",
        "PEAK" | "PEAKD" => "PEAKDETECT",
        "HIR" => "HIRES",
        "AVE" => "AVERAGE",
        "ENV" => "ENVELOPE",
        "RUNST" => "RUNSTOP",
        "SEQ" => "SEQUENCE",
        "DCREJ" => "DCREJECT",
        "HFREJ" => "HFREJ",
        "LFREJ" => "LFREJ",
        "NOISER" => "NOISEREJ",
        other => other,
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_exponent_counts() {
        assert_eq!(parse_count("10000"), Some(10_000));
        assert_eq!(parse_count("1.0000E+04"), Some(10_000));
        assert_eq!(parse_count(" 1.0000E+03 "), Some(1_000));
        assert_eq!(parse_count("10000000"), Some(10_000_000));
    }

    #[test]
    fn rejects_non_counts() {
        assert_eq!(parse_count("MEG"), None);
        assert_eq!(parse_count(""), None);
        assert_eq!(parse_count("-1"), None);
        assert_eq!(parse_count("inf"), None);
    }

    #[test]
    fn validates_rigol_fixed_termination_and_memory_choices() {
        let capabilities = crate::backend::from_idn("RIGOL,DHO924S,SN,00.01.05").capabilities();
        let mut channel = ChannelConfig {
            enabled: true,
            scale: 0.1,
            position: 0.0,
            offset: 0.0,
            coupling: "DC".into(),
            termination_ohms: 1e6,
            bandwidth_hz: 250e6,
            probe_gain: 1.0,
            probe_type: "unknown".into(),
            wave_type: String::new(),
            frequency_hz: 0.0,
        };
        assert!(
            validate_section(&ConfigSection::Channel(0, channel.clone()), &capabilities).is_ok()
        );
        channel.termination_ohms = 50.0;
        assert!(
            validate_section(&ConfigSection::Channel(0, channel), &capabilities)
                .unwrap_err()
                .to_string()
                .contains("input termination")
        );

        let horizontal = HorizontalConfig {
            scale: 1e-3,
            position: 50.0,
            record_length: 50_000_000,
        };
        assert!(validate_section(&ConfigSection::Horizontal(horizontal), &capabilities).is_ok());
    }

    #[test]
    fn current_instrument_values_extend_static_capabilities() {
        let mut capabilities = crate::backend::from_idn("TEKTRONIX,MDO3024,SN,1").capabilities();
        let channel = ChannelConfig {
            enabled: true,
            scale: 0.1,
            position: 0.0,
            offset: 0.0,
            coupling: "CUSTOM".into(),
            termination_ohms: 75.0,
            bandwidth_hz: 123e6,
            probe_gain: 1.0,
            probe_type: "unknown".into(),
            wave_type: String::new(),
            frequency_hz: 0.0,
        };
        let config = InstrumentConfig {
            channels: [channel.clone(), channel.clone(), channel.clone(), channel],
            horizontal: HorizontalConfig {
                scale: 1e-3,
                position: 50.0,
                record_length: 12_345,
            },
            trigger: TriggerConfig {
                mode: "CUSTOM".into(),
                source: "CH1".into(),
                slope: "CUSTOM".into(),
                coupling: "CUSTOM".into(),
                level: 0.0,
            },
            acquisition: AcquisitionConfig {
                mode: "CUSTOM".into(),
                stop_after: "CUSTOM".into(),
                running: true,
            },
        };

        include_current_values(&mut capabilities, &config);
        assert!(capabilities.channel_couplings.contains(&"CUSTOM".into()));
        assert!(capabilities.record_lengths.contains(&12_345));
        assert!(validate_section(
            &ConfigSection::Channel(0, config.channels[0].clone()),
            &capabilities
        )
        .is_ok());
        assert!(
            validate_section(&ConfigSection::Horizontal(config.horizontal), &capabilities).is_ok()
        );
    }
}
