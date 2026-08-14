//! TOML-backed config storage under `%APPDATA%\<app>`.

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use std::path::{Path, PathBuf};

/// `%APPDATA%\<app>`, falling back to the current directory if APPDATA is unset.
pub fn app_dir(app: &str) -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(app)
}

pub fn ensure_app_dir(app: &str) -> Result<PathBuf> {
    let dir = app_dir(app);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

/// Directory holding the executable, used to locate bundled vendor binaries.
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Load a config file, falling back to `Default` when it is missing or corrupt.
///
/// A corrupt file is preserved as `<name>.bad` so a bad edit is recoverable
/// rather than silently overwritten on the next save.
pub fn load_or_default<T: DeserializeOwned + Default>(path: &Path) -> T {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return T::default(),
        Err(e) => {
            tracing::warn!("could not read {}: {e}", path.display());
            return T::default();
        }
    };
    match toml::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("{} is not valid TOML: {e}", path.display());
            let _ = std::fs::rename(path, path.with_extension("bad"));
            T::default()
        }
    }
}

/// Write a config file atomically, so a crash mid-write cannot truncate it.
pub fn save<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let text = toml::to_string_pretty(value).context("serializing config")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}
