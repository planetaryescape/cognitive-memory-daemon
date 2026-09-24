//! Runtime identity and filesystem paths.
//!
//! The instance name is a data boundary: socket, PID file, SQLite database,
//! cache, logs, config, and bridge token state all resolve from the same
//! identity before any durable handle is opened.

use std::path::{Path, PathBuf};

pub const PROD_INSTANCE_NAME: &str = "cognitive-memory";
pub const DEV_INSTANCE_NAME: &str = "cognitive-memory-dev";

/// Build-default instance name. Release builds use the production identity;
/// debug/test builds use a dev identity so local work cannot accidentally
/// attach to a user's release daemon or database.
pub fn default_instance_name() -> &'static str {
    if cfg!(debug_assertions) {
        DEV_INSTANCE_NAME
    } else {
        PROD_INSTANCE_NAME
    }
}

/// Resolve the active runtime identity.
///
/// `COGNITIVE_MEMORY_INSTANCE` is the canonical override. Empty values are
/// ignored so accidentally-exported blank env vars do not collapse paths.
pub fn app_instance_name() -> String {
    std::env::var("COGNITIVE_MEMORY_INSTANCE")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| default_instance_name().to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePaths {
    pub instance: String,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub log_dir: PathBuf,
    pub config_path: PathBuf,
    pub socket_path: PathBuf,
    pub pid_path: PathBuf,
    pub db_path: PathBuf,
    pub bridge_token_path: PathBuf,
    pub bridge_port_path: PathBuf,
    pub model_cache_dir: PathBuf,
    pub daemon_log_path: PathBuf,
}

impl RuntimePaths {
    pub fn resolve() -> Self {
        Self::for_instance(app_instance_name())
    }

    pub fn for_instance(instance: impl Into<String>) -> Self {
        let instance = instance.into();
        let base_config = std::env::var_os("COGNITIVE_MEMORY_CONFIG_DIR")
            .map(PathBuf::from)
            .or_else(dirs::config_dir)
            .expect("config dir resolvable")
            .join(&instance);
        let base_data = std::env::var_os("COGNITIVE_MEMORY_DATA_DIR")
            .map(PathBuf::from)
            .or_else(dirs::data_dir)
            .expect("data dir resolvable")
            .join(&instance);
        let base_runtime = std::env::var_os("COGNITIVE_MEMORY_RUNTIME_DIR")
            .map(PathBuf::from)
            .or_else(dirs::runtime_dir)
            .or_else(dirs::data_local_dir)
            .or_else(dirs::data_dir)
            .expect("runtime dir resolvable")
            .join(&instance);
        let base_cache = std::env::var_os("COGNITIVE_MEMORY_CACHE_DIR")
            .map(PathBuf::from)
            .or_else(dirs::cache_dir)
            .expect("cache dir resolvable")
            .join(&instance);
        let base_log = std::env::var_os("COGNITIVE_MEMORY_LOG_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join("Library").join("Logs")))
            .or_else(dirs::cache_dir)
            .expect("log dir resolvable")
            .join(&instance);

        let socket_path = std::env::var_os("COGNITIVE_MEMORY_SOCKET_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| base_runtime.join("cm.sock"));

        Self {
            instance,
            config_path: base_config.join("config.toml"),
            bridge_token_path: base_config.join("bridge-token.json"),
            bridge_port_path: base_config.join("bridge-port"),
            db_path: base_data.join("data.db"),
            pid_path: base_runtime.join("cm-daemon.pid"),
            socket_path,
            model_cache_dir: base_cache.join("models"),
            daemon_log_path: base_log.join("daemon.log"),
            config_dir: base_config,
            data_dir: base_data,
            runtime_dir: base_runtime,
            cache_dir: base_cache,
            log_dir: base_log,
        }
    }

    /// Create directories that hold private runtime state. Directories are
    /// repaired to 0700 on Unix; on non-Unix platforms this is best-effort.
    pub fn ensure_private_dirs(&self) -> std::io::Result<()> {
        for dir in [
            &self.config_dir,
            &self.data_dir,
            &self.runtime_dir,
            &self.cache_dir,
            &self.log_dir,
            &self.model_cache_dir,
        ] {
            ensure_private_dir(dir)?;
        }
        Ok(())
    }
}

pub fn ensure_private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        if perms.mode() & 0o777 != 0o700 {
            perms.set_mode(0o700);
            std::fs::set_permissions(path, perms)?;
        }
    }
    Ok(())
}

pub fn secure_private_file_if_exists(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        if perms.mode() & 0o777 != 0o600 {
            perms.set_mode(0o600);
            std::fs::set_permissions(path, perms)?;
        }
    }
    Ok(())
}

pub fn secure_private_socket(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(path)?.permissions();
        if perms.mode() & 0o777 != 0o700 {
            perms.set_mode(0o700);
            std::fs::set_permissions(path, perms)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn instance_scopes_every_durable_path() {
        let paths = RuntimePaths::for_instance("cm-test");

        assert_eq!(paths.instance, "cm-test");
        assert!(paths.socket_path.to_string_lossy().contains("cm-test"));
        assert!(paths.pid_path.to_string_lossy().contains("cm-test"));
        assert!(paths.db_path.to_string_lossy().contains("cm-test"));
        assert!(paths.config_path.to_string_lossy().contains("cm-test"));
        assert!(paths
            .bridge_token_path
            .to_string_lossy()
            .contains("cm-test"));
        assert!(paths.daemon_log_path.to_string_lossy().contains("cm-test"));
        assert!(paths.model_cache_dir.to_string_lossy().contains("cm-test"));
    }

    #[test]
    fn private_directory_permissions_are_repaired() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("runtime");
        std::fs::create_dir_all(&dir).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&dir).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&dir, perms).unwrap();
        }

        ensure_private_dir(&dir).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
    }
}
