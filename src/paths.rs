use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlintPaths {
    root: PathBuf,
}

impl GlintPaths {
    pub fn discover() -> Result<Self> {
        let home = std::env::var_os("HOME").context("HOME is not set")?;
        Ok(Self::from_home(home))
    }

    pub fn from_home(home: impl Into<PathBuf>) -> Self {
        Self {
            root: home.into().join(".glint"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn home(&self) -> &Path {
        self.root
            .parent()
            .expect("Glint root is always constructed below a home directory")
    }

    pub fn config(&self) -> PathBuf {
        self.root.join("config.yaml")
    }

    pub fn auth(&self) -> PathBuf {
        self.root.join("auth.json")
    }

    pub fn plugins(&self) -> PathBuf {
        self.root.join("plugins")
    }

    pub fn mcp(&self) -> PathBuf {
        self.root.join("mcp")
    }

    pub fn sessions(&self) -> PathBuf {
        self.root.join("sessions")
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        sync::{Mutex, OnceLock},
    };

    use super::GlintPaths;

    #[test]
    fn fixed_paths_are_all_below_dot_glint() {
        let paths = GlintPaths::from_home("/users/alice");

        assert_eq!(paths.root(), Path::new("/users/alice/.glint"));
        assert_eq!(paths.home(), Path::new("/users/alice"));
        assert_eq!(paths.config(), Path::new("/users/alice/.glint/config.yaml"));
        assert_eq!(paths.auth(), Path::new("/users/alice/.glint/auth.json"));
        assert_eq!(paths.plugins(), Path::new("/users/alice/.glint/plugins"));
        assert_eq!(paths.mcp(), Path::new("/users/alice/.glint/mcp"));
        assert_eq!(paths.sessions(), Path::new("/users/alice/.glint/sessions"));
    }

    #[test]
    fn discover_ignores_legacy_config_environment_overrides() {
        let _environment = environment_lock().lock().unwrap();
        let _restore = EnvironmentRestore::capture(&["GLINT_CONFIG", "XDG_CONFIG_HOME"]);
        unsafe {
            std::env::set_var("GLINT_CONFIG", "/tmp/legacy-glint.yaml");
            std::env::set_var("XDG_CONFIG_HOME", "/tmp/legacy-xdg");
        }

        let home = std::env::var_os("HOME").expect("test process has HOME");
        let paths = GlintPaths::discover().unwrap();

        assert_eq!(paths.root(), Path::new(&home).join(".glint"));
        assert_eq!(paths.config(), Path::new(&home).join(".glint/config.yaml"));
    }

    fn environment_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct EnvironmentRestore {
        values: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvironmentRestore {
        fn capture(names: &[&'static str]) -> Self {
            Self {
                values: names
                    .iter()
                    .map(|name| (*name, std::env::var_os(name)))
                    .collect(),
            }
        }
    }

    impl Drop for EnvironmentRestore {
        fn drop(&mut self) {
            for (name, value) in &self.values {
                unsafe {
                    if let Some(value) = value {
                        std::env::set_var(name, value);
                    } else {
                        std::env::remove_var(name);
                    }
                }
            }
        }
    }
}
