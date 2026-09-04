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
    use std::path::Path;

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
}
