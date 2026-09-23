use crate::backend::Backend;
use crate::scpi::{ScpiError, ScpiSession};

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

pub fn read_config(
    session: &mut ScpiSession,
    backend: &dyn Backend,
) -> Result<InstrumentConfig, ScpiError> {
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
    })
}

pub fn apply_section(
    session: &mut ScpiSession,
    backend: &dyn Backend,
    section: &ConfigSection,
) -> Result<(), ScpiError> {
    match section {
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
    response
        .trim_matches('"')
        .parse()
        .map_err(|_| ScpiError::Parse(format!("{command} returned {response:?}")))
}

fn query_u64(session: &mut ScpiSession, command: &str) -> Result<u64, ScpiError> {
    let response = session.query(command)?;
    parse_count(&response)
        .ok_or_else(|| ScpiError::Parse(format!("{command} returned {response:?}")))
}

/// A Tektronix reports a record length as `10000`, a Rigol as `1.0000E+04`.
fn parse_count(text: &str) -> Option<u64> {
    let text = text.trim();
    if let Ok(value) = text.parse::<u64>() {
        return Some(value);
    }
    let value = text.parse::<f64>().ok()?;
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    Some(value.round() as u64)
}

pub fn query_bool(session: &mut ScpiSession, command: &str) -> Result<bool, ScpiError> {
    let response = clean_enum(&session.query(command)?);
    match response.as_str() {
        "1" | "ON" | "RUN" => Ok(true),
        "0" | "OFF" | "STOP" => Ok(false),
        _ => Err(ScpiError::Parse(format!("{command} returned {response:?}"))),
    }
}

fn clean_enum(value: &str) -> String {
    let value = value.trim().trim_matches('"').to_ascii_uppercase();
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
        _ => &value,
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
}
