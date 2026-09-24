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
    let Ok(text) = fs::read_to_string(path()).or_else(|_| fs::read_to_string(legacy_path())) else {
        return Prefs::default();
    };
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
    prefs
}

pub fn save(prefs: &Prefs) {
    let path = path();
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
