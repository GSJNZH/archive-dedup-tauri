use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
pub struct JobConfig {
    #[serde(default)]
    pub archive: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub copy_mode: String,
    #[serde(default = "default_workers")]
    pub workers: usize,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub auto_verify: bool,
}

fn default_workers() -> usize {
    8
}

#[derive(Debug, Clone, Serialize)]
pub struct Progress {
    pub stage: String,
    pub message: String,
    pub percent: f64,
    pub archived: u64,
    pub unique: u64,
    pub size_gb: String,
    #[serde(default)]
    pub copied: u64,
    #[serde(default)]
    pub failed: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobDone {
    pub ok: bool,
    pub message: String,
    pub report: String,
    pub archived: u64,
    pub unique: u64,
    pub size_gb: String,
    #[serde(default)]
    pub copied: u64,
    #[serde(default)]
    pub failed: u64,
}
