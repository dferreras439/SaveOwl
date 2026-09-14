use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::{Path, PathBuf}};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub gpu_threshold_percent: f64,
    pub activation_samples: u32,
    pub end_samples: u32,
    pub poll_ms: u64,
    pub max_snapshot_file_mb: u64,
    pub upstream_dir: Option<PathBuf>,
    pub ignore_processes: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gpu_threshold_percent: 25.0,
            activation_samples: 3,
            end_samples: 5,
            poll_ms: 1000,
            max_snapshot_file_mb: 512,
            upstream_dir: None,
            ignore_processes: vec![
                "dwm.exe".into(), "explorer.exe".into(), "obs64.exe".into(),
                "chrome.exe".into(), "msedge.exe".into(), "firefox.exe".into(),
            ],
        }
    }
}

impl Config {
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
            return toml::from_str(&text).context("parsing config.toml");
        }
        if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
        let cfg = Self::default();
        fs::write(path, toml::to_string_pretty(&cfg)?)?;
        Ok(cfg)
    }
}
