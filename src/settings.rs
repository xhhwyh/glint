use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Clone, Debug, Default)]
pub struct ProjectPermissions {
    pub allow: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ProjectSettings {
    pub root: PathBuf,
    pub permissions: ProjectPermissions,
}

impl ProjectSettings {
    pub fn load() -> Self {
        let root = project_root();
        let permissions = load_project_permissions(&root).unwrap_or_default();
        Self { root, permissions }
    }

    pub fn allows_bash(&self, command: &str) -> bool {
        self.permissions
            .allow
            .iter()
            .any(|permission| bash_permission_matches(permission, command))
    }

    pub fn allow_bash_prefix(&mut self, command: &str) -> Result<String> {
        let prefix = bash_permission_prefix(command)
            .with_context(|| format!("failed to derive Bash permission prefix from {command}"))?;
        persist_bash_allow_prefix(&self.root, &prefix)?;
        if !self.permissions.allow.contains(&prefix) {
            self.permissions.allow.push(prefix.clone());
        }
        Ok(prefix)
    }
}

#[derive(Default, Deserialize, Serialize)]
struct SettingsFile {
    #[serde(default)]
    permissions: PermissionsFile,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

#[derive(Default, Deserialize, Serialize)]
struct PermissionsFile {
    #[serde(default)]
    allow: Vec<String>,
    #[serde(flatten)]
    extra: Map<String, Value>,
}

pub fn project_root() -> PathBuf {
    let mut current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    loop {
        if current.join("Cargo.toml").exists() {
            return current;
        }
        if !current.pop() {
            return std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        }
    }
}

pub fn bash_permission_prefix(command: &str) -> Option<String> {
    let words = command
        .split_whitespace()
        .take(2)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    match words.as_slice() {
        [] => None,
        [one] => Some(format!("Bash({one} *)")),
        [one, two] => Some(format!("Bash({one} {two} *)")),
        _ => None,
    }
}

fn bash_permission_matches(permission: &str, command: &str) -> bool {
    let Some(prefix) = permission
        .strip_prefix("Bash(")
        .and_then(|value| value.strip_suffix(" *)"))
    else {
        return false;
    };

    command == prefix || command.starts_with(&format!("{prefix} "))
}

fn load_project_permissions(root: &Path) -> Result<ProjectPermissions> {
    let path = settings_path(root);
    if is_git_tracked(root, &path) {
        return Ok(ProjectPermissions::default());
    }
    if !path.exists() {
        return Ok(ProjectPermissions::default());
    }

    let file =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let settings: SettingsFile = serde_json::from_str(&file)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(ProjectPermissions {
        allow: settings.permissions.allow,
    })
}

fn persist_bash_allow_prefix(root: &Path, prefix: &str) -> Result<()> {
    let path = settings_path(root);
    let mut settings = if path.exists() {
        let file = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        serde_json::from_str::<SettingsFile>(&file)
            .with_context(|| format!("failed to parse {}", path.display()))?
    } else {
        SettingsFile::default()
    };

    if !settings
        .permissions
        .allow
        .iter()
        .any(|entry| entry == prefix)
    {
        settings.permissions.allow.push(prefix.to_owned());
    }

    let parent = path
        .parent()
        .context("settings path did not have a parent")?;
    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    let content =
        serde_json::to_string_pretty(&settings).context("failed to serialize settings")?;
    fs::write(&path, format!("{content}\n"))
        .with_context(|| format!("failed to write {}", path.display()))
}

fn is_git_tracked(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    Command::new("git")
        .arg("ls-files")
        .arg("--error-unmatch")
        .arg(relative)
        .current_dir(root)
        .output()
        .is_ok_and(|output| output.status.success())
}

fn settings_path(root: &Path) -> PathBuf {
    root.join(".glint").join("settings.local.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn derives_bash_prefix_from_first_two_words() {
        assert_eq!(
            bash_permission_prefix("cargo test --lib").as_deref(),
            Some("Bash(cargo test *)")
        );
        assert_eq!(
            bash_permission_prefix("git status --short").as_deref(),
            Some("Bash(git status *)")
        );
    }

    #[test]
    fn bash_prefix_matches_same_starting_words() {
        assert!(bash_permission_matches(
            "Bash(cargo test *)",
            "cargo test --lib"
        ));
        assert!(!bash_permission_matches(
            "Bash(cargo test *)",
            "cargo check"
        ));
    }

    #[test]
    fn preserves_unknown_settings_when_persisting_prefix() {
        let root = std::env::temp_dir().join(format!("glint-settings-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".glint")).unwrap();
        fs::write(
            settings_path(&root),
            json!({ "other": true, "permissions": { "allow": ["Bash(git status *)"], "other": 1 } }).to_string(),
        )
        .unwrap();

        persist_bash_allow_prefix(&root, "Bash(cargo test *)").unwrap();
        persist_bash_allow_prefix(&root, "Bash(cargo test *)").unwrap();

        let value: Value =
            serde_json::from_str(&fs::read_to_string(settings_path(&root)).unwrap()).unwrap();
        assert_eq!(value["other"], true);
        assert_eq!(value["permissions"]["other"], 1);
        assert_eq!(
            value["permissions"]["allow"],
            json!(["Bash(git status *)", "Bash(cargo test *)"])
        );
        let _ = fs::remove_dir_all(root);
    }
}
