pub mod anchor;
pub mod chat;
pub mod config;
pub mod connectors;
pub mod consolidate;
pub mod digest;
pub mod eval;
pub mod evidence;
pub mod extract;
pub mod health;
pub mod heartbeats;
pub mod insights;
pub mod intent;
pub mod links;
pub mod merge;
pub mod models_config;
pub mod prepass;
pub mod profile;
pub mod project;
pub mod proposals;
pub mod replay;
pub mod report;
pub mod scope;
pub mod segmenter;
pub mod self_score;
pub mod sessionizer;
pub mod setup;
pub mod storage;
pub mod timeref;
pub mod types;
pub mod usage;

use std::path::PathBuf;

/// Per-platform data dir: XDG / `%APPDATA%` / `~/Library/Application Support`.
pub fn data_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "chronicle").map(|d| d.data_dir().to_path_buf())
}
