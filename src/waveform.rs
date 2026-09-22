use crate::scpi::{ScpiError, ScpiSession};

#[derive(Clone, Debug)]
pub struct ChannelTrace {
    pub channel: String,
    pub x_unit: String,
    pub y_unit: String,
    /// (time_s, volts)
    pub points: Vec<[f64; 2]>,
}

#[derive(Debug, thiserror::Error)]
pub enum WaveformError {
    #[error(transparent)]
    Scpi(#[from] ScpiError),
    #[error("parse: {0}")]
    Parse(String),
}

/// Scaling factors from a single `WFMOutpre?` response.
#[derive(Debug, Clone)]
struct Preamble {
    x_unit: String,
    xincr: f64,
    xzero: f64,
    y_unit: String,
    ymult: f64,
    yoff: f64,
    yzero: f64,
}

/// Pull one analog channel as scaled (t, V) points.
pub fn fetch_channel(session: &mut ScpiSession, ch: &str) -> Result<ChannelTrace, WaveformError> {
    session.write(&format!("DATA:SOURCE {ch}"))?;
    session.write("DATA:ENC RIBINARY")?;
    session.write("DATA:WIDTH 2")?;
    session.write("DATA:START 1")?;
    let record_len = session.query("HORizontal:RECOrdlength?")?;
    session.write(&format!("DATA:STOP {}", record_len.trim()))?;

    // One round trip instead of seven; each individual WFMOutpre:<field>?
    // costs ~25 ms on this instrument.
    let pre = parse_preamble(&session.query("WFMOutpre?")?)?;

    let raw = session.query_binary_block("CURVE?")?;
    if raw.len() % 2 != 0 {
        return Err(WaveformError::Parse(format!(
            "odd CURVE byte count {}",
            raw.len()
        )));
    }

    let points = raw
        .chunks_exact(2)
        .enumerate()
        .map(|(i, c)| {
            let level = i16::from_be_bytes([c[0], c[1]]) as f64;
            [
                pre.xzero + pre.xincr * i as f64,
                pre.yzero + pre.ymult * (level - pre.yoff),
            ]
        })
        .collect();

    Ok(ChannelTrace {
        channel: ch.to_string(),
        x_unit: pre.x_unit,
        y_unit: pre.y_unit,
        points,
    })
}

/// `WFMOutpre?` is semicolon separated, but the field count is not stable:
/// this MDO3024 returns 22 fields, with model/firmware-specific entries both
/// before (`PT_ORDER`) and after (`TIM;ANALOG;…`) the scaling values. Anchor
/// instead on the three quoted fields, which are always WFID, XUNIT and YUNIT;
/// the scaling values sit immediately after their unit.
fn parse_preamble(resp: &str) -> Result<Preamble, WaveformError> {
    let fields = split_unquoted(resp);
    let quoted: Vec<usize> = fields
        .iter()
        .enumerate()
        .filter(|(_, f)| f.trim().starts_with('"'))
        .map(|(i, _)| i)
        .collect();

    if quoted.len() < 3 {
        return Err(WaveformError::Parse(format!(
            "WFMOutpre? has {} quoted fields, expected WFID/XUNIT/YUNIT: {resp}",
            quoted.len()
        )));
    }
    let (x_at, y_at) = (quoted[1], quoted[2]);
    if y_at + 3 >= fields.len() {
        return Err(WaveformError::Parse(format!(
            "WFMOutpre? truncated: {resp}"
        )));
    }

    let num = |i: usize, name: &str| -> Result<f64, WaveformError> {
        fields[i]
            .trim()
            .parse::<f64>()
            .map_err(|_| WaveformError::Parse(format!("{name} = {:?}", fields[i])))
    };

    Ok(Preamble {
        x_unit: unquote(&fields[x_at]),
        xincr: num(x_at + 1, "XINCR")?,
        xzero: num(x_at + 2, "XZERO")?,
        y_unit: unquote(&fields[y_at]),
        ymult: num(y_at + 1, "YMULT")?,
        yoff: num(y_at + 2, "YOFF")?,
        yzero: num(y_at + 3, "YZERO")?,
    })
}

fn split_unquoted(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                cur.push(c);
            }
            ';' if !in_quotes => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').to_string()
}

pub fn demo_trace(ch: &str) -> ChannelTrace {
    let n = 2000usize;
    let points = (0..n)
        .map(|i| {
            let t = i as f64 * 4e-9;
            let v = (2.0 * std::f64::consts::PI * 5e6 * t).sin()
                + 0.15 * (2.0 * std::f64::consts::PI * 17e6 * t).sin();
            [t, v]
        })
        .collect();
    ChannelTrace {
        channel: ch.to_string(),
        x_unit: "s".into(),
        y_unit: "V".into(),
        points,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured verbatim from a TEKTRONIX MDO3024, FV:v1.30 (22 fields).
    const REAL: &str = r#"2;16;BIN;RI;MSB;"Ch1, DC coupling, 100.0V/div, 4.000us/div, 10000 points, Sample mode";10000;Y;LINEA;"s";4.0000E-9;-20.0000E-6;0;"V";15.6250E-3;12.8000E+3;0.0E+0;TIM;ANALOG;0.0E+0;0.0E+0;0.0E+0"#;

    #[test]
    fn parses_real_preamble() {
        let p = parse_preamble(REAL).unwrap();
        assert_eq!(p.x_unit, "s");
        assert_eq!(p.y_unit, "V");
        assert_eq!(p.xincr, 4.0e-9);
        assert_eq!(p.xzero, -20.0e-6);
        assert_eq!(p.ymult, 15.625e-3);
        assert_eq!(p.yoff, 12800.0);
        assert_eq!(p.yzero, 0.0);
    }

    #[test]
    fn semicolon_inside_wfid_does_not_split() {
        let s = r#"2;16;BIN;RI;MSB;"Ch1; odd; label";10000;Y;LINEA;"s";1.0;0.0;0;"V";2.0;3.0;4.0"#;
        let p = parse_preamble(s).unwrap();
        assert_eq!(p.ymult, 2.0);
        assert_eq!(p.yoff, 3.0);
        assert_eq!(p.yzero, 4.0);
    }

    /// Shorter layout without the MDO trailing fields must parse identically.
    #[test]
    fn tolerates_missing_trailing_fields() {
        let s =
            r#"2;16;BIN;RI;MSB;"Ch1";10000;Y;"s";4.0E-9;-20.0E-6;0;"V";15.625E-3;12.8E+3;0.0E+0"#;
        let p = parse_preamble(s).unwrap();
        assert_eq!(p.xincr, 4.0e-9);
        assert_eq!(p.yoff, 12800.0);
        assert_eq!(p.y_unit, "V");
    }
}
