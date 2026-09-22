use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use crate::waveform::ChannelTrace;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
}

impl ExportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Json => "json",
        }
    }
}

/// CSV is one row per sample: `channel,t,v`. JSON is a document of traces.
pub fn render(traces: &[ChannelTrace], idn: Option<&str>, format: ExportFormat) -> String {
    match format {
        ExportFormat::Csv => csv(traces, idn),
        ExportFormat::Json => json(traces, idn),
    }
}

pub fn write_file(
    path: &Path,
    traces: &[ChannelTrace],
    idn: Option<&str>,
    format: ExportFormat,
) -> std::io::Result<()> {
    fs::write(path, render(traces, idn, format))
}

fn csv(traces: &[ChannelTrace], idn: Option<&str>) -> String {
    let mut out = String::new();
    if let Some(idn) = idn {
        let _ = writeln!(out, "# idn={}", idn.replace('\n', " "));
    }
    for t in traces {
        let _ = writeln!(
            out,
            "# {} x_unit={} y_unit={} points={}",
            t.channel,
            t.x_unit,
            t.y_unit,
            t.points.len()
        );
    }
    let _ = writeln!(out, "channel,t,v");
    for t in traces {
        for p in &t.points {
            let _ = writeln!(out, "{},{},{}", t.channel, p[0], p[1]);
        }
    }
    out
}

fn json(traces: &[ChannelTrace], idn: Option<&str>) -> String {
    let mut out = String::from("{\n");
    if let Some(idn) = idn {
        let _ = writeln!(out, "  \"idn\": {},", json_str(idn));
    }
    out.push_str("  \"traces\": [\n");
    for (i, t) in traces.iter().enumerate() {
        let _ = writeln!(out, "    {{");
        let _ = writeln!(out, "      \"channel\": {},", json_str(&t.channel));
        let _ = writeln!(out, "      \"x_unit\": {},", json_str(&t.x_unit));
        let _ = writeln!(out, "      \"y_unit\": {},", json_str(&t.y_unit));
        out.push_str("      \"points\": [");
        for (j, p) in t.points.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            let _ = write!(out, "[{},{}]", p[0], p[1]);
        }
        out.push_str("]\n    }");
        if i + 1 != traces.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
}

fn json_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::waveform::ChannelTrace;

    fn sample() -> Vec<ChannelTrace> {
        vec![
            ChannelTrace {
                channel: "CH1".into(),
                x_unit: "s".into(),
                y_unit: "V".into(),
                points: vec![[0.0, 1.5], [1e-6, -0.25]],
            },
            ChannelTrace {
                channel: "CH2".into(),
                x_unit: "s".into(),
                y_unit: "V".into(),
                points: vec![[0.0, 0.0]],
            },
        ]
    }

    #[test]
    fn csv_has_header_and_one_row_per_sample() {
        let text = render(&sample(), Some("TEK,MDO"), ExportFormat::Csv);
        assert!(text.contains("# idn=TEK,MDO"));
        assert!(text.contains("channel,t,v"));
        assert!(text.contains("CH1,0,1.5"));
        assert!(text.contains("CH2,0,0"));
        assert_eq!(
            text.lines()
                .filter(|l| !l.starts_with('#') && *l != "channel,t,v")
                .count(),
            3
        );
    }

    #[test]
    fn json_lists_each_trace() {
        let text = render(&sample(), Some("scope"), ExportFormat::Json);
        assert!(text.contains("\"idn\": \"scope\""));
        assert!(text.contains("\"channel\": \"CH1\""));
        assert!(text.contains("[0,1.5]"));
        assert!(text.contains("\"channel\": \"CH2\""));
    }
}
