use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::timestamp::{CaptureTime, CaptureTimeSource};
use crate::waveform::ChannelTrace;

/// Loaded offline capture.
#[derive(Debug, Clone)]
pub struct LoadedCapture {
    pub traces: Vec<ChannelTrace>,
    pub idn: Option<String>,
    pub captured_at: Option<CaptureTime>,
    #[allow(dead_code)]
    pub description: String,
}

#[derive(Deserialize)]
struct JsonExportFile {
    idn: Option<String>,
    captured_at: Option<String>,
    captured_at_source: Option<String>,
    traces: Option<Vec<JsonTrace>>,
}

#[derive(Deserialize)]
struct JsonTrace {
    channel: String,
    x_unit: Option<String>,
    y_unit: Option<String>,
    points: Vec<[f64; 2]>,
}

/// Parses an exported JSON or CSV capture file back into traces and metadata.
pub fn load_capture(path: &Path) -> Result<LoadedCapture, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read file {}: {}", path.display(), e))?;

    let is_json = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("json"))
        .unwrap_or(false)
        || content.trim_start().starts_with('{');

    if is_json {
        load_json(&content, path)
    } else {
        load_csv(&content, path)
    }
}

fn load_json(content: &str, path: &Path) -> Result<LoadedCapture, String> {
    let parsed: JsonExportFile = serde_json::from_str(content)
        .map_err(|e| format!("Failed to parse JSON capture: {}", e))?;

    let traces = parsed
        .traces
        .unwrap_or_default()
        .into_iter()
        .map(|t| ChannelTrace {
            channel: t.channel,
            x_unit: t.x_unit.unwrap_or_else(|| "s".to_string()),
            y_unit: t.y_unit.unwrap_or_else(|| "V".to_string()),
            points: t.points,
        })
        .collect();

    let captured_at = parsed.captured_at.map(|iso| {
        let source = if parsed.captured_at_source.as_deref() == Some("instrument") {
            CaptureTimeSource::Instrument
        } else {
            CaptureTimeSource::Host
        };
        CaptureTime { iso, source }
    });

    let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("capture");

    Ok(LoadedCapture {
        traces,
        idn: parsed.idn,
        captured_at,
        description: format!("Loaded {}", filename),
    })
}

fn load_csv(content: &str, path: &Path) -> Result<LoadedCapture, String> {
    let mut idn = None;
    let mut captured_at = None;
    let mut lines = content.lines().peekable();

    // Parse comments
    while let Some(&line) = lines.peek() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            let comment = trimmed.trim_start_matches('#').trim();
            if let Some(rest) = comment.strip_prefix("idn=") {
                idn = Some(rest.trim().to_string());
            } else if let Some(rest) = comment.strip_prefix("captured_at=") {
                let parts: Vec<&str> = rest.split_whitespace().collect();
                if let Some(iso) = parts.first() {
                    let source = if parts.iter().any(|&p| p == "source=instrument") {
                        CaptureTimeSource::Instrument
                    } else {
                        CaptureTimeSource::Host
                    };
                    captured_at = Some(CaptureTime {
                        iso: iso.to_string(),
                        source,
                    });
                }
            }
            lines.next();
        } else {
            break;
        }
    }

    let header = lines.next().ok_or("Empty CSV file")?;
    let header_parts: Vec<&str> = header.split(',').map(|s| s.trim()).collect();

    let mut traces = Vec::new();

    if header_parts.len() >= 3 && header_parts[0] == "channel" && header_parts[1] == "t" && header_parts[2] == "v" {
        // Long format
        let mut map: std::collections::BTreeMap<String, Vec<[f64; 2]>> = std::collections::BTreeMap::new();
        for line in lines {
            let row: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
            if row.len() >= 3 {
                if let (Ok(t), Ok(v)) = (row[1].parse::<f64>(), row[2].parse::<f64>()) {
                    map.entry(row[0].to_string()).or_default().push([t, v]);
                }
            }
        }
        for (channel, points) in map {
            traces.push(ChannelTrace {
                channel,
                x_unit: "s".to_string(),
                y_unit: "V".to_string(),
                points,
            });
        }
    } else if header_parts.len() >= 2 && header_parts[0] == "t" {
        // Wide format: t, CH1, CH2, ...
        let channel_names: Vec<String> = header_parts[1..].iter().map(|s| s.to_string()).collect();
        let mut points_per_ch: Vec<Vec<[f64; 2]>> = vec![Vec::new(); channel_names.len()];

        for line in lines {
            let row: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
            if let Some(t_str) = row.first() {
                if let Ok(t) = t_str.parse::<f64>() {
                    for (ch_idx, &val_str) in row.iter().skip(1).enumerate() {
                        if ch_idx < points_per_ch.len() {
                            if let Ok(v) = val_str.parse::<f64>() {
                                points_per_ch[ch_idx].push([t, v]);
                            }
                        }
                    }
                }
            }
        }

        for (channel, points) in channel_names.into_iter().zip(points_per_ch) {
            traces.push(ChannelTrace {
                channel,
                x_unit: "s".to_string(),
                y_unit: "V".to_string(),
                points,
            });
        }
    } else {
        return Err("Unrecognized CSV format".to_string());
    }

    let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("capture");

    Ok(LoadedCapture {
        traces,
        idn,
        captured_at,
        description: format!("Loaded {}", filename),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::{render, ExportFormat, ExportOptions};

    #[test]
    fn round_trip_json_capture() {
        let trace = ChannelTrace {
            channel: "CH1".to_string(),
            x_unit: "s".to_string(),
            y_unit: "V".to_string(),
            points: vec![[0.0, 1.0], [0.001, 2.5], [0.002, -1.0]],
        };
        let traces = vec![trace];
        let captured = CaptureTime::host_now();
        let opts = ExportOptions {
            traces: &traces,
            idn: Some("Rigol Technologies,DS1054Z"),
            settings: None,
            format: ExportFormat::Json,
            csv_wide: false,
            cursor_a: None,
            cursor_b: None,
            captured_at: Some(&captured),
        };

        let json_text = render(&opts);
        let path = Path::new("dummy.json");
        let loaded = load_json(&json_text, path).expect("parse json");

        assert_eq!(loaded.traces.len(), 1);
        assert_eq!(loaded.traces[0].channel, "CH1");
        assert_eq!(loaded.traces[0].points.len(), 3);
        assert_eq!(loaded.idn.as_deref(), Some("Rigol Technologies,DS1054Z"));
        assert!(loaded.captured_at.is_some());
    }

    #[test]
    fn round_trip_csv_long_capture() {
        let trace = ChannelTrace {
            channel: "CH2".to_string(),
            x_unit: "s".to_string(),
            y_unit: "V".to_string(),
            points: vec![[0.0, 0.5], [0.005, -0.5]],
        };
        let traces = vec![trace];
        let opts = ExportOptions {
            traces: &traces,
            idn: Some("Tektronix,MDO3024"),
            settings: None,
            format: ExportFormat::Csv,
            csv_wide: false,
            cursor_a: None,
            cursor_b: None,
            captured_at: None,
        };

        let csv_text = render(&opts);
        let path = Path::new("test.csv");
        let loaded = load_csv(&csv_text, path).expect("parse csv");

        assert_eq!(loaded.traces.len(), 1);
        assert_eq!(loaded.traces[0].channel, "CH2");
        assert_eq!(loaded.traces[0].points.len(), 2);
    }
}
