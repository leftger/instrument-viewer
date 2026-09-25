use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use crate::config::InstrumentConfig;
use crate::measure::{self, Measurements};
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

/// Default save-dialog name: `instrument-capture-YYYYMMDD-HHMMSS.<ext>`.
pub fn capture_filename(ext: &str, captured: Option<&crate::timestamp::CaptureTime>) -> String {
    let stamp = captured
        .map(|c| c.filename_stamp())
        .unwrap_or_else(|| crate::timestamp::CaptureTime::host_now().filename_stamp());
    format!("instrument-capture-{stamp}.{ext}")
}

pub struct ExportOptions<'a> {
    pub traces: &'a [ChannelTrace],
    pub idn: Option<&'a str>,
    pub settings: Option<&'a InstrumentConfig>,
    pub format: ExportFormat,
    pub csv_wide: bool,
    pub cursor_a: Option<f64>,
    pub cursor_b: Option<f64>,
    pub captured_at: Option<&'a crate::timestamp::CaptureTime>,
}

/// CSV is long-form `channel,t,v` or wide `t,CH1,CH2,…`. JSON includes
/// measurements and a settings snapshot when provided.
pub fn render(opts: &ExportOptions<'_>) -> String {
    match opts.format {
        ExportFormat::Csv if opts.csv_wide => csv_wide(opts),
        ExportFormat::Csv => csv_long(opts),
        ExportFormat::Json => json(opts),
    }
}

pub fn write_file(path: &Path, opts: &ExportOptions<'_>) -> std::io::Result<()> {
    fs::write(path, render(opts))
}

fn csv_long(opts: &ExportOptions<'_>) -> String {
    let mut out = String::new();
    write_csv_comments(&mut out, opts);
    let _ = writeln!(out, "channel,t,v");
    for t in opts.traces {
        for p in &t.points {
            let _ = writeln!(out, "{},{},{}", t.channel, p[0], p[1]);
        }
    }
    out
}

fn csv_wide(opts: &ExportOptions<'_>) -> String {
    let mut out = String::new();
    write_csv_comments(&mut out, opts);
    out.push('t');
    for t in opts.traces {
        let _ = write!(out, ",{}", t.channel);
    }
    out.push('\n');

    let rows = opts
        .traces
        .iter()
        .map(|t| t.points.len())
        .max()
        .unwrap_or(0);
    for i in 0..rows {
        let t = opts
            .traces
            .iter()
            .find_map(|tr| tr.points.get(i).map(|p| p[0]))
            .unwrap_or(0.0);
        let _ = write!(out, "{t}");
        for tr in opts.traces {
            match tr.points.get(i) {
                Some(p) => {
                    let _ = write!(out, ",{}", p[1]);
                }
                None => out.push(','),
            }
        }
        out.push('\n');
    }
    out
}

fn write_csv_comments(out: &mut String, opts: &ExportOptions<'_>) {
    if let Some(idn) = opts.idn {
        let _ = writeln!(out, "# idn={}", idn.replace('\n', " "));
    }
    if let Some(captured) = opts.captured_at {
        let _ = writeln!(
            out,
            "# captured_at={} source={}",
            captured.iso,
            captured.source.as_str()
        );
    }
    for t in opts.traces {
        let _ = writeln!(
            out,
            "# {} x_unit={} y_unit={} points={}",
            t.channel,
            t.x_unit,
            t.y_unit,
            t.points.len()
        );
        if let Some(m) = measure::measure(t) {
            let _ = writeln!(
                out,
                "# {} min={} max={} pkpk={} mean={} rms={} period={:?} freq={:?}",
                t.channel, m.min, m.max, m.pk_pk, m.mean, m.rms, m.period_s, m.frequency_hz
            );
        }
    }
}

fn json(opts: &ExportOptions<'_>) -> String {
    let mut out = String::from("{\n");
    if let Some(idn) = opts.idn {
        let _ = writeln!(out, "  \"idn\": {},", json_str(idn));
    }
    if let Some(captured) = opts.captured_at {
        let _ = writeln!(out, "  \"captured_at\": {},", json_str(&captured.iso));
        let _ = writeln!(
            out,
            "  \"captured_at_source\": {},",
            json_str(captured.source.as_str())
        );
    }
    if let (Some(a), Some(b)) = (opts.cursor_a, opts.cursor_b) {
        let dt = b - a;
        let _ = writeln!(out, "  \"cursors\": {{");
        let _ = writeln!(out, "    \"t1\": {a},");
        let _ = writeln!(out, "    \"t2\": {b},");
        let _ = writeln!(out, "    \"dt\": {dt},");
        if dt.abs() > f64::EPSILON {
            let _ = writeln!(out, "    \"inv_dt\": {},", 1.0 / dt);
        }
        out.push_str("  },\n");
    }
    if let Some(settings) = opts.settings {
        out.push_str("  \"settings\": ");
        out.push_str(&settings_json(settings));
        out.push_str(",\n");
    }
    out.push_str("  \"traces\": [\n");
    for (i, t) in opts.traces.iter().enumerate() {
        let _ = writeln!(out, "    {{");
        let _ = writeln!(out, "      \"channel\": {},", json_str(&t.channel));
        let _ = writeln!(out, "      \"x_unit\": {},", json_str(&t.x_unit));
        let _ = writeln!(out, "      \"y_unit\": {},", json_str(&t.y_unit));
        if let Some(m) = measure::measure(t) {
            out.push_str("      \"measurements\": ");
            out.push_str(&measurements_json(&m));
            out.push_str(",\n");
        }
        out.push_str("      \"points\": [");
        for (j, p) in t.points.iter().enumerate() {
            if j > 0 {
                out.push(',');
            }
            let _ = write!(out, "[{},{}]", p[0], p[1]);
        }
        out.push_str("]\n    }");
        if i + 1 != opts.traces.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
}

fn measurements_json(m: &Measurements) -> String {
    let mut out = String::from("{\n");
    let _ = writeln!(out, "        \"min\": {},", m.min);
    let _ = writeln!(out, "        \"max\": {},", m.max);
    let _ = writeln!(out, "        \"pk_pk\": {},", m.pk_pk);
    let _ = writeln!(out, "        \"mean\": {},", m.mean);
    let _ = writeln!(out, "        \"rms\": {},", m.rms);
    match m.period_s {
        Some(p) => {
            let _ = writeln!(out, "        \"period_s\": {p},");
        }
        None => out.push_str("        \"period_s\": null,\n"),
    }
    match m.frequency_hz {
        Some(f) => {
            let _ = writeln!(out, "        \"frequency_hz\": {f}");
        }
        None => out.push_str("        \"frequency_hz\": null\n"),
    }
    out.push_str("      }");
    out
}

fn settings_json(c: &InstrumentConfig) -> String {
    let mut out = String::from("{\n");
    let _ = writeln!(
        out,
        "    \"horizontal\": {{ \"scale_s_div\": {}, \"position_pct\": {}, \"record_length\": {} }},",
        c.horizontal.scale, c.horizontal.position, c.horizontal.record_length
    );
    let _ = writeln!(
        out,
        "    \"trigger\": {{ \"mode\": {}, \"source\": {}, \"slope\": {}, \"coupling\": {}, \"level_v\": {} }},",
        json_str(&c.trigger.mode),
        json_str(&c.trigger.source),
        json_str(&c.trigger.slope),
        json_str(&c.trigger.coupling),
        c.trigger.level
    );
    let _ = writeln!(
        out,
        "    \"acquisition\": {{ \"mode\": {}, \"stop_after\": {}, \"running\": {} }},",
        json_str(&c.acquisition.mode),
        json_str(&c.acquisition.stop_after),
        c.acquisition.running
    );
    if !c.output_pair.is_empty() {
        let _ = writeln!(out, "    \"output_pair\": {},", json_str(&c.output_pair));
    }
    out.push_str("    \"channels\": [\n");
    for (i, ch) in c.channels.iter().enumerate() {
        let att = if ch.probe_gain > 0.0 {
            1.0 / ch.probe_gain
        } else {
            1.0
        };
        let _ = writeln!(
            out,
            "      {{ \"name\": \"CH{}\", \"enabled\": {}, \"scale_v_div\": {}, \"position_div\": {}, \"offset_v\": {}, \"coupling\": {}, \"termination_ohm\": {}, \"bandwidth_hz\": {}, \"probe_attenuation\": {}, \"probe_type\": {} }}{}",
            i + 1,
            ch.enabled,
            ch.scale,
            ch.position,
            ch.offset,
            json_str(&ch.coupling),
            ch.termination_ohms,
            ch.bandwidth_hz,
            att,
            json_str(&ch.probe_type),
            if i + 1 == c.channels.len() { "" } else { "," }
        );
    }
    out.push_str("    ]\n  }");
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
                points: vec![[0.0, 0.0], [1e-6, 0.5]],
            },
        ]
    }

    fn opts(format: ExportFormat, wide: bool) -> String {
        let captured = crate::timestamp::CaptureTime {
            iso: "2026-09-23T20:26:03".into(),
            source: crate::timestamp::CaptureTimeSource::Instrument,
        };
        render(&ExportOptions {
            traces: &sample(),
            idn: Some("TEK,MDO"),
            settings: None,
            format,
            csv_wide: wide,
            cursor_a: None,
            cursor_b: None,
            captured_at: Some(&captured),
        })
    }

    #[test]
    fn csv_has_header_and_one_row_per_sample() {
        let text = opts(ExportFormat::Csv, false);
        assert!(text.contains("# idn=TEK,MDO"));
        assert!(text.contains("# captured_at=2026-09-23T20:26:03 source=instrument"));
        assert!(text.contains("channel,t,v"));
        assert!(text.contains("CH1,0,1.5"));
        assert!(text.contains("CH2,0,0"));
        assert_eq!(
            text.lines()
                .filter(|l| !l.starts_with('#') && *l != "channel,t,v")
                .count(),
            4
        );
    }

    #[test]
    fn csv_wide_has_column_per_channel() {
        let text = opts(ExportFormat::Csv, true);
        assert!(text.contains("t,CH1,CH2"));
        assert!(text.contains("0,1.5,0"));
        assert!(text.contains("0.000001,-0.25,0.5") || text.contains("1e-6,-0.25,0.5"));
    }

    #[test]
    fn json_lists_each_trace_and_measurements() {
        let text = opts(ExportFormat::Json, false);
        assert!(text.contains("\"idn\": \"TEK,MDO\""));
        assert!(text.contains("\"captured_at\": \"2026-09-23T20:26:03\""));
        assert!(text.contains("\"captured_at_source\": \"instrument\""));
        assert!(text.contains("\"channel\": \"CH1\""));
        assert!(text.contains("[0,1.5]"));
        assert!(text.contains("\"measurements\""));
        assert!(text.contains("\"pk_pk\""));
    }

    #[test]
    fn capture_filename_includes_local_timestamp() {
        let captured = crate::timestamp::CaptureTime {
            iso: "2026-09-23T20:26:03".into(),
            source: crate::timestamp::CaptureTimeSource::Instrument,
        };
        let name = capture_filename("csv", Some(&captured));
        assert_eq!(name, "instrument-capture-20260923-202603.csv");
        let host = capture_filename("png", None);
        assert!(
            host.starts_with("instrument-capture-") && host.ends_with(".png"),
            "{host}"
        );
    }
}
