use crate::domain::Price;
use serde::Deserialize;
use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    #[serde(rename = "$schema")]
    pub schema: Option<String>,
    pub source: Option<String>,
    pub timezone: Option<String>,
    pub format: Option<String>,
    pub json: Option<bool>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub order: Option<String>,
    pub mode: Option<String>,
    pub debug: Option<bool>,
    pub debug_samples: Option<usize>,
    pub single_thread: Option<bool>,
    pub breakdown: Option<bool>,
    pub color: Option<bool>,
    pub no_color: Option<bool>,
    pub compact: Option<bool>,
    pub no_cost: Option<bool>,
    pub by_agent: Option<bool>,
    pub instances: Option<bool>,
    pub session_id: Option<String>,
    pub project: Option<String>,
    pub instance: Option<String>,
    pub project_aliases: Option<HashMap<String, String>>,
    pub speed: Option<String>,
    pub sections: Option<Vec<String>>,
    pub active: Option<bool>,
    pub recent: Option<bool>,
    pub token_limit: Option<String>,
    pub session_length_hours: Option<u32>,
    pub visual_burn_rate: Option<String>,
    pub cost_source: Option<String>,
    pub refresh_interval: Option<u64>,
    pub context_low_threshold: Option<u8>,
    pub context_medium_threshold: Option<u8>,
    pub cache: Option<bool>,
    #[serde(default)]
    pub prices: HashMap<String, Price>,
}

pub fn load(explicit: Option<&Path>) -> Result<Config, String> {
    if let Some(path) = explicit {
        return read_config(path);
    }
    for path in candidates() {
        if path.is_file() {
            return read_config(&path);
        }
    }
    Ok(Config::default())
}

fn candidates() -> Vec<PathBuf> {
    if let Some(path) = env::var_os("USCT_CONFIG") {
        return vec![PathBuf::from(path)];
    }
    let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let config_home = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    vec![
        config_home.join("usct/config.json"),
        home.join(".usct.json"),
    ]
}

fn read_config(path: &Path) -> Result<Config, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("cannot read config {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid config {}: {error}", path.display()))
}
