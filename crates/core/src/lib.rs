pub mod chat;
pub mod config;
pub mod digest;
pub mod eval;
pub mod merge;
pub mod sessionizer;
pub mod storage;
pub mod timeref;
pub mod types;

use std::path::PathBuf;

/// Per-platform data dir: XDG / `%APPDATA%` / `~/Library/Application Support`.
pub fn data_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "chronicle").map(|d| d.data_dir().to_path_buf())
}
