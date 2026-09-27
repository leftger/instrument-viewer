use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// Generic log entry representing a data sample from any bench instrument.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRecord {
    pub timestamp: String,
    pub elapsed_secs: f64,
    pub sample_idx: u64,
    pub instrument: String,
    pub channel: String,
    pub parameter: String,
    pub value: f64,
    pub unit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary_value: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secondary_unit: Option<String>,
}

impl LogRecord {
    pub fn csv_header() -> &'static str {
        "timestamp,elapsed_secs,sample_idx,instrument,channel,parameter,value,unit,secondary_value,secondary_unit"
    }

    pub fn to_csv_row(&self) -> String {
        format!(
            "{},{:.3},{},\"{}\",\"{}\",\"{}\",{:.6},\"{}\",{},\"{}\"\n",
            self.timestamp,
            self.elapsed_secs,
            self.sample_idx,
            self.instrument.replace('"', "\"\""),
            self.channel.replace('"', "\"\""),
            self.parameter.replace('"', "\"\""),
            self.value,
            self.unit.replace('"', "\"\""),
            self.secondary_value
                .map(|v| format!("{:.6}", v))
                .unwrap_or_default(),
            self.secondary_unit
                .as_deref()
                .unwrap_or_default()
                .replace('"', "\"\""),
        )
    }
}

/// Continuous streaming data logger for CSV and JSON Lines (JSONL).
pub struct DataLogger {
    csv_file: Option<(PathBuf, File)>,
    json_file: Option<(PathBuf, File)>,
    start_time: Option<Instant>,
    sample_count: u64,
}

impl Default for DataLogger {
    fn default() -> Self {
        Self::new()
    }
}

impl DataLogger {
    pub fn new() -> Self {
        Self {
            csv_file: None,
            json_file: None,
            start_time: None,
            sample_count: 0,
        }
    }

    pub fn is_logging(&self) -> bool {
        self.csv_file.is_some() || self.json_file.is_some()
    }

    pub fn sample_count(&self) -> u64 {
        self.sample_count
    }

    pub fn elapsed_secs(&self) -> f64 {
        self.start_time
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0)
    }

    #[allow(dead_code)]
    pub fn csv_path(&self) -> Option<&Path> {
        self.csv_file.as_ref().map(|(p, _)| p.as_path())
    }

    #[allow(dead_code)]
    pub fn json_path(&self) -> Option<&Path> {
        self.json_file.as_ref().map(|(p, _)| p.as_path())
    }

    /// Starts logging to the given optional paths.
    pub fn start(
        &mut self,
        csv_path: Option<PathBuf>,
        json_path: Option<PathBuf>,
    ) -> io::Result<()> {
        self.stop();

        if let Some(path) = csv_path {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let file_exists = path.exists();
            let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
            if !file_exists || fs::metadata(&path)?.len() == 0 {
                writeln!(file, "{}", LogRecord::csv_header())?;
                file.flush()?;
            }
            self.csv_file = Some((path, file));
        }

        if let Some(path) = json_path {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            self.json_file = Some((path, file));
        }

        self.start_time = Some(Instant::now());
        self.sample_count = 0;
        Ok(())
    }

    /// Stops continuous logging and closes files.
    pub fn stop(&mut self) {
        if let Some((_, mut file)) = self.csv_file.take() {
            let _ = file.flush();
        }
        if let Some((_, mut file)) = self.json_file.take() {
            let _ = file.flush();
        }
        self.start_time = None;
    }

    /// Logs a single measurement sample to active files.
    pub fn log(&mut self, record: &LogRecord) -> io::Result<()> {
        if !self.is_logging() {
            return Ok(());
        }

        if let Some((_, file)) = &mut self.csv_file {
            file.write_all(record.to_csv_row().as_bytes())?;
            file.flush()?;
        }

        if let Some((_, file)) = &mut self.json_file {
            let json_line = serde_json::to_string(record).map_err(io::Error::other)?;
            writeln!(file, "{}", json_line)?;
            file.flush()?;
        }

        self.sample_count += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datalogger_writes_csv_and_jsonl() {
        let temp_dir = std::env::temp_dir().join(format!("iv_test_{}", std::process::id()));
        let _ = fs::create_dir_all(&temp_dir);
        let csv_path = temp_dir.join("test_log.csv");
        let json_path = temp_dir.join("test_log.jsonl");

        let mut logger = DataLogger::new();
        assert!(!logger.is_logging());

        logger
            .start(Some(csv_path.clone()), Some(json_path.clone()))
            .expect("start logging");
        assert!(logger.is_logging());

        let rec = LogRecord {
            timestamp: "2026-09-27T03:00:00Z".to_string(),
            elapsed_secs: 1.234,
            sample_idx: 1,
            instrument: "DM3058".to_string(),
            channel: "CH1".to_string(),
            parameter: "VOLT:DC".to_string(),
            value: 4.9982,
            unit: "V".to_string(),
            secondary_value: None,
            secondary_unit: None,
        };

        logger.log(&rec).expect("log record");
        assert_eq!(logger.sample_count(), 1);

        logger.stop();
        assert!(!logger.is_logging());

        let csv_content = fs::read_to_string(&csv_path).expect("read csv");
        assert!(csv_content.starts_with(LogRecord::csv_header()));
        assert!(csv_content.contains("4.9982"));

        let json_content = fs::read_to_string(&json_path).expect("read jsonl");
        assert!(json_content.contains("\"parameter\":\"VOLT:DC\""));
        assert!(json_content.contains("4.9982"));

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
