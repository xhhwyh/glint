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
    use std::{path::Path, process::Command};

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
        if std::env::var_os("GLINT_PATHS_CHILD_TEST").is_some() {
            let paths = GlintPaths::discover().unwrap();
            assert_eq!(paths.root(), Path::new("/tmp/glint-paths-child/.glint"));
            assert_eq!(
                paths.config(),
                Path::new("/tmp/glint-paths-child/.glint/config.yaml")
            );
            return;
        }

        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "paths::tests::discover_ignores_legacy_config_environment_overrides",
            ])
            .env("GLINT_PATHS_CHILD_TEST", "1")
            .env("HOME", "/tmp/glint-paths-child")
            .env("GLINT_CONFIG", "/tmp/legacy-glint.yaml")
            .env("XDG_CONFIG_HOME", "/tmp/legacy-xdg")
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "child path assertion failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
