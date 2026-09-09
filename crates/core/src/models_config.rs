//! `<data dir>/models.toml` (m31): cloud model backends and the job-kind
//! routing table. Mode 0600, atomic write like `gcal::Tokens::save` /
//! `McpConfig::save` — this file carries API keys and must never live in
//! `config.toml`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;

/// AI job kinds that write free text or chat and are the default target of
/// the "use for writing and chat" preset.
pub const WRITING_KINDS: &[&str] = &[
    "chat",
    "narrative",
    "standup",
    "journal",
    "task_description",
    "checkpoint",
    "suggest_task",
    "name_task",
];

/// Every job kind that can be routed, including the ones the "use for
/// everything" preset also flips: scorer/segmenter never call a model, so
/// these are the full set that ever reaches a backend.
pub const ALL_KINDS: &[&str] = &[
    "chat",
    "narrative",
    "standup",
    "journal",
    "task_description",
    "checkpoint",
    "suggest_task",
    "name_task",
    "consolidate",
    "derive",
    "live",
];

fn default_max_usd() -> f64 {
    2.0
}

/// One cloud backend: a name, a wire shape, and the key to speak it with.
/// `claude_code` has no key: it spawns the user's own Claude Code login,
/// and `command` names the binary when it is not `claude` on `PATH`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BackendCfg {
    pub kind: BackendKind,
    pub model: String,
    #[serde(default)]
    pub api_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

impl BackendCfg {
    /// First 7 chars + "…" + last 4, for the UI — the key itself is never
    /// printed anywhere else. Short keys (under 12 chars) mask fully.
    pub fn masked_key(&self) -> String {
        if self.kind == BackendKind::ClaudeCode {
            return "your Claude Code login".to_string();
        }
        let chars: Vec<char> = self.api_key.chars().collect();
        if chars.len() < 12 {
            return "••••".to_string();
        }
        let head: String = chars[..7].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}…{tail}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendKind {
    Anthropic,
    OpenAiCompat,
    /// Headless Claude Code (`claude -p`) on the user's own subscription.
    ClaudeCode,
}

/// A one-click routing preset. `WritingAndChat` is the proposed default
/// when a backend is first added; `Everything` also routes the derive
/// tiers, which stay local otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preset {
    WritingAndChat,
    Everything,
}

/// Named backends plus a job-kind → backend routing table, and the daily
/// spend cap that flips routes back to local once reached.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelsConfig {
    #[serde(default)]
    pub backends: BTreeMap<String, BackendCfg>,
    /// job kind -> backend name; a route naming a missing backend behaves
    /// as if unset (the job runs local).
    #[serde(default)]
    pub routes: BTreeMap<String, String>,
    #[serde(default = "default_max_usd")]
    pub max_usd_per_day: f64,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            backends: BTreeMap::new(),
            routes: BTreeMap::new(),
            max_usd_per_day: default_max_usd(),
        }
    }
}

impl ModelsConfig {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("models.toml")
    }

    /// Missing file means no cloud backend configured yet: defaults, not
    /// an error.
    pub fn load(data_dir: &Path) -> anyhow::Result<Self> {
        let path = Self::path(data_dir);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let cfg: Self =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        Ok(cfg)
    }

    /// Atomic write, mode 0600: temp file in the same directory, then
    /// rename, so a daemon loading mid-save never sees a torn file and the
    /// key material is never world-readable even briefly.
    pub fn save(&self, data_dir: &Path) -> anyhow::Result<()> {
        let path = Self::path(data_dir);
        let text = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        // A leftover from an interrupted save may carry other permissions;
        // `mode` only applies on create.
        let _ = std::fs::remove_file(&tmp);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let written = (|| -> anyhow::Result<()> {
            let mut file = opts.open(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            Ok(())
        })();
        if let Err(err) = written {
            let _ = std::fs::remove_file(&tmp);
            return Err(err);
        }
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// The backend a job kind should run on, if its route names one that
    /// still exists. A dangling route (backend removed) means local.
    pub fn route_for(&self, kind: &str) -> Option<(&str, &BackendCfg)> {
        let name = self.routes.get(kind)?;
        self.backends.get(name).map(|cfg| (name.as_str(), cfg))
    }

    pub fn is_cloud(&self, kind: &str) -> bool {
        self.route_for(kind).is_some()
    }

    /// Job kinds whose route currently resolves to a live backend.
    pub fn cloud_kinds(&self) -> Vec<String> {
        self.routes
            .keys()
            .filter(|k| self.route_for(k).is_some())
            .cloned()
            .collect()
    }

    /// One-click routing: `WritingAndChat` points `WRITING_KINDS` at
    /// `backend`; `Everything` also flips consolidate/derive/live.
    pub fn apply_preset(&mut self, preset: Preset, backend: &str) {
        for kind in WRITING_KINDS {
            self.routes.insert((*kind).to_string(), backend.to_string());
        }
        if preset == Preset::Everything {
            for kind in ["consolidate", "derive", "live"] {
                self.routes.insert(kind.to_string(), backend.to_string());
            }
        }
    }

    /// Drops the backend and every route pointing at it (they fall back to
    /// local rather than dangling silently).
    pub fn remove_backend(&mut self, name: &str) {
        self.backends.remove(name);
        self.routes.retain(|_, v| v != name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("chronicle-models-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn backend(model: &str, key: &str) -> BackendCfg {
        BackendCfg {
            kind: BackendKind::Anthropic,
            model: model.to_string(),
            api_key: key.to_string(),
            base_url: None,
            command: None,
        }
    }

    #[test]
    fn missing_file_loads_default_with_two_dollar_cap() {
        let dir = temp_dir("missing");
        let cfg = ModelsConfig::load(&dir).unwrap();
        assert_eq!(cfg, ModelsConfig::default());
        assert_eq!(cfg.max_usd_per_day, 2.0);
        assert!(cfg.backends.is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_round_trips_and_is_private() {
        let dir = temp_dir("roundtrip");
        let mut cfg = ModelsConfig::default();
        cfg.backends.insert(
            "anthropic".to_string(),
            backend("claude-opus-5", "sk-ant-abcdefghijklmnop"),
        );
        cfg.routes
            .insert("chat".to_string(), "anthropic".to_string());

        cfg.save(&dir).unwrap();
        assert!(!ModelsConfig::path(&dir).with_extension("toml.tmp").exists());

        let loaded = ModelsConfig::load(&dir).unwrap();
        assert_eq!(loaded, cfg);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(ModelsConfig::path(&dir))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        // Overwrite keeps working (the temp file is recreated each time).
        cfg.save(&dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn route_for_ignores_a_dangling_route() {
        let mut cfg = ModelsConfig::default();
        cfg.routes.insert("chat".to_string(), "ghost".to_string());
        assert_eq!(cfg.route_for("chat"), None);
        assert!(!cfg.is_cloud("chat"));
        assert!(cfg.cloud_kinds().is_empty());

        cfg.backends.insert(
            "ghost".to_string(),
            backend("claude-haiku-4.5", "sk-ant-abcdefghijklmnop"),
        );
        let (name, resolved) = cfg.route_for("chat").unwrap();
        assert_eq!(name, "ghost");
        assert_eq!(resolved.model, "claude-haiku-4.5");
        assert!(cfg.is_cloud("chat"));
        assert_eq!(cfg.cloud_kinds(), vec!["chat".to_string()]);
    }

    #[test]
    fn apply_preset_and_remove_backend() {
        let mut cfg = ModelsConfig::default();
        cfg.backends.insert(
            "anthropic".to_string(),
            backend("claude-sonnet-5", "sk-ant-abcdefghijklmnop"),
        );

        cfg.apply_preset(Preset::WritingAndChat, "anthropic");
        for kind in WRITING_KINDS {
            assert_eq!(cfg.routes.get(*kind).map(String::as_str), Some("anthropic"));
        }
        assert!(!cfg.routes.contains_key("derive"));

        cfg.apply_preset(Preset::Everything, "anthropic");
        for kind in ["consolidate", "derive", "live"] {
            assert_eq!(cfg.routes.get(kind).map(String::as_str), Some("anthropic"));
        }
        assert_eq!(cfg.cloud_kinds().len(), ALL_KINDS.len());

        cfg.remove_backend("anthropic");
        assert!(cfg.backends.is_empty());
        assert!(cfg.routes.is_empty());
        assert!(cfg.cloud_kinds().is_empty());
    }

    #[test]
    fn masked_key_shows_head_and_tail_only() {
        let long = backend("claude-opus-5", "sk-ant-api03-abcdefghijklmnop");
        let masked = long.masked_key();
        assert!(masked.starts_with("sk-ant-"));
        assert!(masked.ends_with("mnop"));
        assert!(!masked.contains("abcdefgh"));

        let short = backend("claude-opus-5", "short-key");
        assert_eq!(short.masked_key(), "••••");
    }
}
