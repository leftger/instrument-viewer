use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct Prefs {
    pub host: String,
    pub port: String,
    pub auto_interval: f64,
    pub csv_wide: bool,
    pub scroll_zooms: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            host: "169.254.6.252".into(),
            port: "4000".into(),
            auto_interval: 2.0,
            csv_wide: false,
            scroll_zooms: true,
        }
    }
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn path() -> PathBuf {
    home_dir()
        .join(".config")
        .join("instrument-viewer")
        .join("prefs")
}

fn legacy_path() -> PathBuf {
    home_dir().join(".config").join("mdo-viewer").join("prefs")
}

pub fn load() -> Prefs {
    load_from(&path()).unwrap_or_else(|| load_from(&legacy_path()).unwrap_or_default())
}

/// Read preferences from `path`; `None` when the file cannot be read.
pub fn load_from(path: &std::path::Path) -> Option<Prefs> {
    let text = fs::read_to_string(path).ok()?;
    let mut prefs = Prefs::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        match k.trim() {
            "host" => prefs.host = v.trim().to_string(),
            "port" => prefs.port = v.trim().to_string(),
            "auto_interval" => {
                if let Ok(x) = v.trim().parse::<f64>() {
                    prefs.auto_interval = x.clamp(0.5, 30.0);
                }
            }
            "csv_wide" => prefs.csv_wide = v.trim() == "true" || v.trim() == "1",
            "scroll_zooms" => prefs.scroll_zooms = v.trim() == "true" || v.trim() == "1",
            _ => {}
        }
    }
    Some(prefs)
}

pub fn save(prefs: &Prefs) {
    save_to(&path(), prefs);
}

/// Write preferences to `path`, creating the directory if needed.
pub fn save_to(path: &std::path::Path, prefs: &Prefs) {
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let body = format!(
        "host={}\nport={}\nauto_interval={}\ncsv_wide={}\nscroll_zooms={}\n",
        prefs.host, prefs.port, prefs.auto_interval, prefs.csv_wide, prefs.scroll_zooms
    );
    let _ = fs::write(path, body);
}

pub fn argv_has(flag: &str) -> bool {
    std::env::args().any(|a| a == flag)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("iv-prefs-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn defaults_match_the_documented_ones() {
        let prefs = Prefs::default();
        assert_eq!(prefs.host, "169.254.6.252");
        assert_eq!(prefs.port, "4000");
        assert_eq!(prefs.auto_interval, 2.0);
        assert!(!prefs.csv_wide);
        assert!(prefs.scroll_zooms);
    }

    #[test]
    fn a_missing_file_has_no_preferences() {
        assert!(load_from(&temp_file("does-not-exist")).is_none());
    }

    #[test]
    fn parses_comments_blanks_and_unknown_keys() {
        let path = temp_file("parsed");
        fs::write(
            &path,
            "# a comment\n\nhost = 10.0.0.5\nport=5025\nunknown=1\nno-equals-here\n",
        )
        .unwrap();
        let prefs = load_from(&path).expect("prefs");
        assert_eq!(prefs.host, "10.0.0.5");
        assert_eq!(prefs.port, "5025");
        // Untouched keys keep their defaults.
        assert_eq!(prefs.auto_interval, 2.0);
        assert!(prefs.scroll_zooms);
    }

    #[test]
    fn booleans_accept_true_and_one() {
        let path = temp_file("bools-on");
        fs::write(&path, "csv_wide=true\nscroll_zooms=1\n").unwrap();
        let prefs = load_from(&path).unwrap();
        assert!(prefs.csv_wide);
        assert!(prefs.scroll_zooms);

        let path = temp_file("bools-off");
        fs::write(&path, "csv_wide=yes\nscroll_zooms=false\n").unwrap();
        let prefs = load_from(&path).unwrap();
        assert!(!prefs.csv_wide);
        assert!(!prefs.scroll_zooms);
    }

    #[test]
    fn auto_interval_is_clamped_and_bad_numbers_ignored() {
        let path = temp_file("interval-low");
        fs::write(&path, "auto_interval=0.1\n").unwrap();
        assert_eq!(load_from(&path).unwrap().auto_interval, 0.5);

        let path = temp_file("interval-high");
        fs::write(&path, "auto_interval=99\n").unwrap();
        assert_eq!(load_from(&path).unwrap().auto_interval, 30.0);

        let path = temp_file("interval-bad");
        fs::write(&path, "auto_interval=soon\n").unwrap();
        assert_eq!(load_from(&path).unwrap().auto_interval, 2.0);
    }

    #[test]
    fn saving_round_trips_and_creates_the_directory() {
        let dir = std::env::temp_dir().join(format!("iv-prefs-save-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let path = dir.join("nested").join("prefs");
        let prefs = Prefs {
            host: "usb:0699:0408:SN#0".into(),
            port: "5555".into(),
            auto_interval: 0.5,
            csv_wide: true,
            scroll_zooms: false,
        };
        save_to(&path, &prefs);
        assert!(path.exists());
        let loaded = load_from(&path).expect("round trip");
        assert_eq!(loaded.host, "usb:0699:0408:SN#0");
        assert_eq!(loaded.port, "5555");
        assert_eq!(loaded.auto_interval, 0.5);
        assert!(loaded.csv_wide);
        assert!(!loaded.scroll_zooms);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn argv_has_only_matches_exact_flags() {
        // The test harness' own argv holds flags like `--nocapture`, so probe
        // with one that cannot be present.
        assert!(!argv_has("--definitely-not-a-real-flag"));
    }
}
