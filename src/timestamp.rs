use crate::backend::InstrumentKind;
use crate::scpi::ScpiSession;

/// When a capture was taken, and whether the clock came from the instrument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureTime {
    /// Local ISO-8601 without timezone: `2026-09-23T20:26:03`.
    pub iso: String,
    pub source: CaptureTimeSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureTimeSource {
    Instrument,
    Host,
}

impl CaptureTimeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Instrument => "instrument",
            Self::Host => "host",
        }
    }
}

impl CaptureTime {
    pub fn host_now() -> Self {
        Self {
            iso: chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string(),
            source: CaptureTimeSource::Host,
        }
    }

    /// Prefer `SYST:DATE?` / `SYST:TIME?` on a power supply; otherwise the PC clock.
    /// Other instruments are not queried: a missing header can time out and wedge them.
    pub fn for_session(session: &mut ScpiSession, kind: InstrumentKind) -> Self {
        if kind == InstrumentKind::Supply {
            if let Some(iso) = query_syst_clock(session) {
                return Self {
                    iso,
                    source: CaptureTimeSource::Instrument,
                };
            }
        }
        Self::host_now()
    }

    /// Compact stamp for default save names: `20260923-202603`.
    pub fn filename_stamp(&self) -> String {
        filename_stamp(&self.iso)
    }
}

fn query_syst_clock(session: &mut ScpiSession) -> Option<String> {
    let date = session.query("SYST:DATE?").ok()?;
    let time = session.query("SYST:TIME?").ok()?;
    format_syst(&date, &time)
}

fn format_syst(date: &str, time: &str) -> Option<String> {
    let d = scpi_numbers(date);
    let t = scpi_numbers(time);
    let (year, month, day) = (to_u32(d.first()?)?, to_u32(d.get(1)?)?, to_u32(d.get(2)?)?);
    let (hour, minute, second) = (
        to_u32(t.first()?)?,
        to_u32(t.get(1)?)?,
        to_u32(t.get(2)?)?.min(59),
    );
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}"
    ))
}

fn scpi_numbers(text: &str) -> Vec<f64> {
    text.split(|c: char| c == ',' || c.is_whitespace())
        .filter_map(|part| {
            let part = part.trim().trim_start_matches('+');
            if part.is_empty() {
                None
            } else {
                part.parse().ok()
            }
        })
        .collect()
}

fn to_u32(value: &f64) -> Option<u32> {
    if !value.is_finite() || *value < 0.0 {
        None
    } else {
        Some(value.round() as u32)
    }
}

fn filename_stamp(iso: &str) -> String {
    iso.chars()
        .filter(|c| c.is_ascii_digit())
        .collect::<String>()
        .chars()
        .take(14)
        .collect::<String>()
        .chars()
        .enumerate()
        .fold(String::new(), |mut out, (i, c)| {
            if i == 8 {
                out.push('-');
            }
            out.push(c);
            out
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_keysight_date_and_time() {
        assert_eq!(
            format_syst("2026,9,23", "20,26,3").as_deref(),
            Some("2026-09-23T20:26:03")
        );
        assert_eq!(
            format_syst("+2026,+09,+23", "8,5,0.4").as_deref(),
            Some("2026-09-23T08:05:00")
        );
        assert_eq!(format_syst("-113,\"Undefined header\"", "20,26,3"), None);
    }

    #[test]
    fn filename_stamp_strips_iso_separators() {
        assert_eq!(filename_stamp("2026-09-23T20:26:03"), "20260923-202603");
    }
}
