//! Runtime configuration loader.
//!
//! All settings are read exactly once at startup, merged into a single
//! immutable [`Config`] value, and passed by reference everywhere. There is
//! no second read path (lesson from agentmemory #456 / #469 — the dimension
//! guard read `process.env` while the rest of the codebase used
//! `getMergedEnv()`, masking the bug for weeks).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use serde::{Deserialize, Serialize};

/// Top-level runtime configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Root data directory holding `wiki/`, `raw/`, `db/`, `models/`, `logs/`.
    pub data_dir: PathBuf,
    /// HTTP bind address used by `ai-memory serve`.
    pub bind: String,
    /// Per-subsystem log filter (overridable by `RUST_LOG`).
    pub log_level: String,
    /// M8 retention-sweep parameters. The defaults give an ~80-day
    /// "survival floor" for unused episodic content (above the cold
    /// threshold), followed by ~180 days of soft-delete buffer before
    /// hard-deletion. Tune `decay.lambda` down to slow decay or
    /// `decay.cold_threshold` to evict more / less aggressively.
    pub decay: ai_memory_store::DecayParams,
    /// Privacy-strip tuning. Built-in patterns always run; this section
    /// lets the operator extend or punch holes in them.
    pub sanitize: ai_memory_core::SanitizeConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            bind: "127.0.0.1:49374".into(),
            log_level: "info".into(),
            decay: ai_memory_store::DecayParams::default(),
            sanitize: ai_memory_core::SanitizeConfig::default(),
        }
    }
}

impl Config {
    /// Load the merged configuration: defaults → file → env → CLI.
    ///
    /// # Errors
    /// Returns an error if the config file is malformed or any required
    /// field is missing.
    pub fn load(config_path: Option<&Path>, cli_data_dir: Option<PathBuf>) -> Result<Self> {
        // Figure out where the config file *would* live so we can read it
        // before knowing the final data dir. CLI > env > default.
        let probe_data_dir = cli_data_dir.clone().unwrap_or_else(default_data_dir);
        let resolved_config_path = config_path
            .map(PathBuf::from)
            .unwrap_or_else(|| probe_data_dir.join("config.toml"));

        let mut figment = Figment::from(Serialized::defaults(Self::default()));
        if resolved_config_path.exists() {
            figment = figment.merge(Toml::file(&resolved_config_path));
        }
        figment = figment.merge(Env::prefixed("AI_MEMORY_").split("__"));

        let mut config: Config = figment.extract().with_context(|| {
            format!(
                "loading configuration (config file = {})",
                resolved_config_path.display()
            )
        })?;

        // CLI override always wins (figment doesn't see it because clap has
        // already consumed the env var into `cli_data_dir`).
        if let Some(dir) = cli_data_dir {
            config.data_dir = dir;
        }

        config.data_dir = canonicalise_or_keep(&config.data_dir);

        Ok(config)
    }
}

fn default_data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ai-memory")
}

fn canonicalise_or_keep(p: &Path) -> PathBuf {
    if let Ok(canon) = p.canonicalize() {
        return canon;
    }
    // Path may not exist yet (init hasn't run). Canonicalise the parent
    // and rejoin so logs and downstream comparisons still see the truth.
    if let (Some(parent), Some(name)) = (p.parent(), p.file_name())
        && let Ok(canon_parent) = parent.canonicalize()
    {
        return canon_parent.join(name);
    }
    p.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn defaults_have_canonical_endings() {
        let cfg = Config::default();
        assert!(cfg.data_dir.ends_with("ai-memory"));
        assert_eq!(cfg.bind, "127.0.0.1:49374");
        assert_eq!(cfg.log_level, "info");
    }

    #[test]
    fn cli_override_wins() {
        let tmp = TempDir::new().unwrap();
        let cli_dir = tmp.path().join("override");
        let cfg = Config::load(None, Some(cli_dir.clone())).unwrap();
        assert_eq!(
            cfg.data_dir,
            // We don't expect the directory to exist yet, so the
            // canonicalise-parent fallback will return parent + name.
            cli_dir
                .parent()
                .and_then(|p| p.canonicalize().ok())
                .map(|c| c.join(cli_dir.file_name().unwrap()))
                .unwrap_or(cli_dir)
        );
    }

    #[test]
    fn config_file_overrides_defaults() {
        let tmp = TempDir::new().unwrap();
        let cfg_path = tmp.path().join("config.toml");
        std::fs::write(
            &cfg_path,
            r#"
            bind = "0.0.0.0:9999"
            log_level = "debug"
            "#,
        )
        .unwrap();
        // Use the tmp dir as the data dir so the resolved config path
        // matches what `load` derives. Passing it explicitly keeps the test
        // free of any global env.
        let cfg = Config::load(Some(&cfg_path), Some(tmp.path().to_path_buf())).unwrap();
        assert_eq!(cfg.bind, "0.0.0.0:9999");
        assert_eq!(cfg.log_level, "debug");
    }
}
