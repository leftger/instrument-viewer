//! Compile-time validation of the bundled `profiles/*.yaml` device catalog.
//!
//! A malformed profile (bad YAML, missing idn_matches, unknown `driver`,
//! invalid `parse`/`encoding`/`preamble` values, …) fails `cargo build` with a
//! message pointing at the offending file, instead of silently loading nothing
//! at runtime. The same schema is documented in `profiles/README.md`.

use std::fs;
use std::path::Path;

fn main() {
    let profiles_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("profiles");
    let entries = fs::read_dir(&profiles_dir).unwrap_or_else(|e| {
        panic!("cannot read {}: {e}", profiles_dir.display());
    });

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if ext != "yaml" && ext != "yml" {
            continue;
        }
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        validate(&path, &text);
    }

    // Re-run whenever a profile or this script changes.
    println!("cargo:rerun-if-changed=profiles");
    println!("cargo:rerun-if-changed=build.rs");
}

fn validate(path: &Path, text: &str) {
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(text)
        .unwrap_or_else(|e| panic!("{}: invalid YAML: {e}", path.display()));
    let map = value
        .as_mapping()
        .unwrap_or_else(|| panic!("{}: profile must be a YAML mapping", path.display()));

    // Required: idn_matches, a non-empty list of non-empty strings.
    let idn = map
        .get("idn_matches")
        .and_then(|v| v.as_sequence())
        .filter(|seq| !seq.is_empty())
        .unwrap_or_else(|| panic!("{}: idn_matches must be a non-empty list", path.display()));
    if idn.iter().any(|v| v.as_str().is_none_or(str::is_empty)) {
        panic!(
            "{}: idn_matches entries must be non-empty strings",
            path.display()
        );
    }

    // Required: name, a non-empty string.
    if !map
        .get("name")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty())
    {
        panic!("{}: name must be a non-empty string", path.display());
    }

    // Optional: driver must name a known hand-written driver.
    if let Some(driver) = map.get("driver") {
        let driver = driver
            .as_str()
            .unwrap_or_else(|| panic!("{}: driver must be a string", path.display()));
        match driver {
            "tek" | "rigol" | "ds1000z" | "sds" | "siglent" | "afg" | "keysight" => {}
            other => panic!("{}: unknown driver {other:?}", path.display()),
        }
    }

    // Optional: commands.* settings.
    if let Some(commands) = map.get("commands").and_then(|v| v.as_mapping()) {
        for (key, setting) in commands {
            let Some(setting) = setting.as_mapping() else {
                panic!("{}: commands.{key:?} must be a mapping", path.display());
            };
            if let Some(parse) = setting.get("parse").and_then(|v| v.as_str()) {
                match parse {
                    "f64" | "bool" | "character" => {}
                    other => panic!(
                        "{}: commands.{key:?}.parse {other:?} must be f64, bool, or character",
                        path.display()
                    ),
                }
            }
            for field in ["query", "write"] {
                if let Some(value) = setting.get(field) {
                    if value.as_str().is_none() {
                        panic!(
                            "{}: commands.{key:?}.{field} must be a string",
                            path.display()
                        );
                    }
                }
            }
        }
    }

    // Optional: capabilities sanity checks.
    if let Some(caps) = map.get("capabilities") {
        let Some(caps) = caps.as_mapping() else {
            panic!("{}: capabilities must be a mapping", path.display());
        };
        if let Some(kind) = caps.get("kind").and_then(|v| v.as_str()) {
            match kind {
                "oscilloscope" | "generator" | "supply" => {}
                other => panic!("{}: capabilities.kind {other:?} is unknown", path.display()),
            }
        }
        if let Some(count) = caps.get("channel_count") {
            if !count.is_u64() && !count.is_i64() {
                panic!(
                    "{}: capabilities.channel_count must be an integer",
                    path.display()
                );
            }
        }
    }

    // Optional: waveform encoding/preamble enums.
    if let Some(waveform) = map.get("waveform") {
        let Some(waveform) = waveform.as_mapping() else {
            panic!("{}: waveform must be a mapping", path.display());
        };
        if let Some(encoding) = waveform.get("encoding").and_then(|v| v.as_str()) {
            match encoding {
                "i16be" | "u16le" | "i8" => {}
                other => panic!(
                    "{}: waveform.encoding {other:?} must be i16be, u16le, or i8",
                    path.display()
                ),
            }
        }
        if let Some(preamble) = waveform.get("preamble").and_then(|v| v.as_str()) {
            match preamble {
                "tek" | "rigol" | "sds" => {}
                other => panic!(
                    "{}: waveform.preamble {other:?} must be tek, rigol, or sds",
                    path.display()
                ),
            }
        }
    }
}
