//! Declarative instrument profiles: settings expressed as command templates and
//! waveform transfer expressed as a data format.
//!
//! Most of an instrument backend is *"query this string, parse the reply as a
//! number"* or *"write that string"*. The [`CommandTable`] captures exactly that
//! surface as data, with `{n}` for the 1-based channel number, `{src}` for a
//! trigger source, and `{v}` for the value being written. A backend that only
//! differs in command spelling can be a table instead of a module; genuinely
//! weird instruments keep hand-written `Backend` overrides.

use crate::scpi::{ScpiError, ScpiSession};

/// How a query reply should be interpreted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Parse {
    /// Decimal number, optionally quoted.
    #[default]
    F64,
    /// `0`/`1`, `ON`/`OFF`, `RUN`/`STOP`.
    Bool,
    /// Character or quoted string, uppercased.
    Character,
}

/// One queryable/writable instrument setting.
#[derive(Clone, Copy, Debug, Default)]
pub struct Setting {
    /// Query command template. `None` when the instrument cannot report it.
    pub query: Option<&'static str>,
    /// Write command template. `None` when the setting is read-only.
    pub write: Option<&'static str>,
    /// How the query reply should be parsed.
    pub parse: Parse,
}

impl Setting {
    /// Render the query template for a 1-based channel number.
    pub fn channel_query(&self, n: usize) -> Option<String> {
        self.query.map(|t| t.replace("{n}", &n.to_string()))
    }

    /// Render the write template for a 1-based channel number and value.
    pub fn channel_write(&self, n: usize, value: &str) -> Option<String> {
        self.write
            .map(|t| t.replace("{n}", &n.to_string()).replace("{v}", value))
    }

    /// Render the query template for a trigger source.
    pub fn source_query(&self, source: &str) -> Option<String> {
        self.query.map(|t| t.replace("{src}", source))
    }

    /// Render the write template for a trigger source and value.
    pub fn source_write(&self, source: &str, value: &str) -> Option<String> {
        self.write
            .map(|t| t.replace("{src}", source).replace("{v}", value))
    }
}

/// The settings most instruments disagree about. Scope-style defaults in
/// `config.rs` cover everything else for oscilloscopes.
#[derive(Clone, Debug, Default)]
pub struct CommandTable {
    pub channel_enabled: Option<Setting>,
    pub termination_ohms: Option<Setting>,
    pub bandwidth_hz: Option<Setting>,
    pub probe_type: Option<Setting>,
    pub trigger_level: Option<Setting>,
}

impl CommandTable {
    /// A table covering none of the optional settings.
    pub fn empty() -> Self {
        Self::default()
    }
}

/// Binary sample layout of a waveform transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleEncoding {
    /// Tektronix `RIBinary`, `DATA:WIDTH 2`: 16-bit big-endian signed.
    I16Be,
    /// Rigol `:WAV:FORM WORD`: 16-bit little-endian unsigned.
    U16Le,
    /// Siglent `WF? DAT2`: raw 8-bit two's-complement codes.
    I8TwosComplement,
}

/// Which preamble/scaling scheme a waveform transfer uses. Preamble parsing is
/// vendor-specific; the sample decoding is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreambleKind {
    Tek,
    Rigol,
    Sds,
}

/// A waveform transfer, described as data so a generic fetcher can decode it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaveformFormat {
    pub encoding: SampleEncoding,
    pub preamble: PreambleKind,
}

impl WaveformFormat {
    pub const TEK: Self = Self {
        encoding: SampleEncoding::I16Be,
        preamble: PreambleKind::Tek,
    };
    pub const RIGOL: Self = Self {
        encoding: SampleEncoding::U16Le,
        preamble: PreambleKind::Rigol,
    };
    pub const SDS: Self = Self {
        encoding: SampleEncoding::I8TwosComplement,
        preamble: PreambleKind::Sds,
    };
}

/// Helper for the default `Backend` methods: read a setting through its table
/// entry, or fail with a uniform unsupported-setting error.
pub fn table_setting(backend_name: &str, setting: Option<&Setting>) -> Result<Setting, ScpiError> {
    setting
        .copied()
        .ok_or_else(|| ScpiError::Unsupported(format!("{backend_name} has no such setting")))
}

/// Query one channel-scoped setting from a [`Setting`] table entry.
pub fn query_channel_setting(
    s: &mut ScpiSession,
    backend_name: &str,
    setting: Option<&Setting>,
    n: usize,
) -> Result<String, ScpiError> {
    let setting = table_setting(backend_name, setting)?;
    let cmd = setting
        .channel_query(n)
        .ok_or_else(|| ScpiError::Unsupported(format!("{backend_name} cannot read channel {n}")))?;
    let reply = s.query(&cmd)?;
    // Validate that the declared parser accepts this reply before handing it on.
    validate_reply(setting.parse, &reply)?;
    Ok(reply)
}

fn validate_reply(parse: Parse, reply: &str) -> Result<(), ScpiError> {
    match parse {
        Parse::F64 => crate::scpi::parse_f64(reply).map(|_| ()),
        Parse::Bool => crate::scpi::parse_bool(reply).map(|_| ()),
        Parse::Character => Ok(()),
    }
}

/// Write one channel-scoped setting from a [`Setting`] table entry.
pub fn write_channel_setting(
    s: &mut ScpiSession,
    backend_name: &str,
    setting: Option<&Setting>,
    n: usize,
    value: &str,
) -> Result<(), ScpiError> {
    let setting = table_setting(backend_name, setting)?;
    let cmd = setting
        .channel_write(n, value)
        .ok_or_else(|| ScpiError::Unsupported(format!("{backend_name} cannot set channel {n}")))?;
    s.write(&cmd)
}

/// Query the trigger-level setting for a source.
pub fn query_source_setting(
    s: &mut ScpiSession,
    backend_name: &str,
    setting: Option<&Setting>,
    source: &str,
) -> Result<String, ScpiError> {
    let setting = table_setting(backend_name, setting)?;
    let cmd = setting.source_query(source).ok_or_else(|| {
        ScpiError::Unsupported(format!("{backend_name} has no trigger level for {source}"))
    })?;
    let reply = s.query(&cmd)?;
    validate_reply(setting.parse, &reply)?;
    Ok(reply)
}

/// Write the trigger-level setting for a source.
pub fn write_source_setting(
    s: &mut ScpiSession,
    backend_name: &str,
    setting: Option<&Setting>,
    source: &str,
    value: &str,
) -> Result<(), ScpiError> {
    let setting = table_setting(backend_name, setting)?;
    let cmd = setting.source_write(source, value).ok_or_else(|| {
        ScpiError::Unsupported(format!(
            "{backend_name} cannot set trigger level for {source}"
        ))
    })?;
    s.write(&cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_channel_templates() {
        let setting = Setting {
            query: Some("CH{n}:SCALE?"),
            write: Some("CH{n}:SCALE {v}"),
            parse: Parse::F64,
        };
        assert_eq!(setting.channel_query(2).as_deref(), Some("CH2:SCALE?"));
        assert_eq!(
            setting.channel_write(2, "0.5").as_deref(),
            Some("CH2:SCALE 0.5")
        );
    }

    #[test]
    fn renders_source_templates() {
        let setting = Setting {
            query: Some("TRIGGER:A:LEVEL:{src}?"),
            write: Some("TRIGGER:A:LEVEL:{src} {v}"),
            parse: Parse::F64,
        };
        assert_eq!(
            setting.source_query("CH1").as_deref(),
            Some("TRIGGER:A:LEVEL:CH1?")
        );
        assert_eq!(
            setting.source_write("CH1", "0.5").as_deref(),
            Some("TRIGGER:A:LEVEL:CH1 0.5")
        );
    }
}
