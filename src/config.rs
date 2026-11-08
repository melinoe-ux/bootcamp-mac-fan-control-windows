// WMF Control (Windows Mac Fan Control) - by melinoe-ux (https://github.com/melinoe-ux) - MIT License
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode")]
pub enum FanMode {
    /// Let macOS-style SMC automatic control run the fan.
    Auto,
    /// Keep the fan at (at least) this RPM.
    Constant { rpm: f64 },
    /// Linear ramp: below `low` °C → minimum speed, above `high` °C → maximum.
    Sensor { key: String, low: f64, high: f64 },
}

impl Default for FanMode {
    fn default() -> Self {
        FanMode::Auto
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Fan modes by fan index.
    pub fans: BTreeMap<u8, FanMode>,
    /// Factory minimum speeds, remembered while we have changed them so they can be
    /// restored even after a crash.
    pub saved_min: BTreeMap<u8, f64>,
    pub poll_ms: u64,
    pub fahrenheit: bool,
    pub show_all_sensors: bool,
    /// Minimizing hides the window to the system tray unless this is set.
    pub no_minimize_to_tray: bool,
}

impl Config {
    fn base() -> PathBuf {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
    }

    fn path() -> PathBuf {
        Self::base().join("WMF Control").join("config.json")
    }

    pub fn load() -> Self {
        // Settings written under the old project name are picked up once.
        let old = Self::base().join("MacFan").join("config.json");
        let mut c: Config = std::fs::read_to_string(Self::path())
            .or_else(|_| std::fs::read_to_string(old))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if c.poll_ms < 250 {
            c.poll_ms = 1000;
        }
        c
    }

    pub fn save(&self) {
        let p = Self::path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(p, s);
        }
    }
}
