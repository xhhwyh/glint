use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};

use crate::{config::UserConfig, paths::GlintPaths};

pub trait AtomicFileWriter: Send + Sync {
    fn write(&self, path: &Path, bytes: &[u8], unix_mode: u32) -> Result<()>;
}

pub struct FsAtomicFileWriter;

impl AtomicFileWriter for FsAtomicFileWriter {
    fn write(&self, path: &Path, bytes: &[u8], unix_mode: u32) -> Result<()> {
        let parent = path
            .parent()
            .context("configuration path does not have a parent directory")?;
        let name = path
            .file_name()
            .context("configuration path does not have a file name")?
            .to_string_lossy();
        let temporary = parent.join(format!(".{name}.tmp-{}", uuid::Uuid::new_v4()));
        let existing_permissions = fs::metadata(path)
            .ok()
            .map(|metadata| metadata.permissions());

        let mut file = open_private_file(&temporary, unix_mode)
            .with_context(|| format!("failed to create {}", temporary.display()))?;
        let write_result = (|| -> Result<()> {
            file.write_all(bytes)
                .with_context(|| format!("failed to write {}", temporary.display()))?;
            file.flush()
                .with_context(|| format!("failed to flush {}", temporary.display()))?;
            Ok(())
        })();
        drop(file);
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        let permission_result = match existing_permissions {
            Some(permissions) => fs::set_permissions(&temporary, permissions).with_context(|| {
                format!(
                    "failed to preserve permissions for temporary configuration {}",
                    temporary.display()
                )
            }),
            None => set_new_file_permissions(&temporary, unix_mode),
        };
        if let Err(error) = permission_result {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }

        if let Err(error) = fs::rename(&temporary, path)
            .with_context(|| format!("failed to replace {}", path.display()))
        {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        Ok(())
    }
}

pub trait UserConfigRepository: Send {
    fn path(&self) -> &Path;
    fn load(&self) -> Result<Option<UserConfig>>;
    fn save(&self, config: &UserConfig) -> Result<()>;
}

pub struct UserConfigStore {
    root: PathBuf,
    path: PathBuf,
    writer: Arc<dyn AtomicFileWriter>,
}

impl UserConfigStore {
    pub fn new(paths: GlintPaths) -> Self {
        Self::with_writer(paths, Arc::new(FsAtomicFileWriter))
    }

    pub fn with_writer(paths: GlintPaths, writer: Arc<dyn AtomicFileWriter>) -> Self {
        Self {
            root: paths.root().to_path_buf(),
            path: paths.config(),
            writer,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Option<UserConfig>> {
        let contents = match fs::read_to_string(&self.path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read {}", self.path.display()));
            }
        };
        serde_yaml::from_str(&contents)
            .map(Some)
            .with_context(|| format!("failed to parse {}", self.path.display()))
    }

    pub fn save(&self, config: &UserConfig) -> Result<()> {
        create_private_directory(&self.root)?;
        let contents = serde_yaml::to_string(config)
            .with_context(|| format!("failed to serialize {}", self.path.display()))?;
        self.writer.write(&self.path, contents.as_bytes(), 0o600)
    }
}

impl UserConfigRepository for UserConfigStore {
    fn path(&self) -> &Path {
        self.path()
    }

    fn load(&self) -> Result<Option<UserConfig>> {
        self.load()
    }

    fn save(&self, config: &UserConfig) -> Result<()> {
        self.save(config)
    }
}

#[cfg(unix)]
fn open_private_file(path: &Path, unix_mode: u32) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(unix_mode)
        .open(path)
}

#[cfg(not(unix))]
fn open_private_file(path: &Path, _unix_mode: u32) -> std::io::Result<std::fs::File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

#[cfg(unix)]
fn set_new_file_permissions(path: &Path, unix_mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(unix_mode))
        .with_context(|| format!("failed to restrict permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn set_new_file_permissions(_path: &Path, _unix_mode: u32) -> Result<()> {
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to restrict permissions on {}", path.display()))?;
    }
    Ok(())
}
