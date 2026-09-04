use std::{
    collections::BTreeMap,
    fmt, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::{
    paths::GlintPaths,
    persistence::{AtomicFileWriter, FsAtomicFileWriter, create_private_directory},
};

const KEYRING_SERVICE: &str = "glint";
const KEYRING_PROBE_ID: &str = "glint-probe";
const KEYRING_UNAVAILABLE_DIAGNOSTIC: &str = "The system credential store is unavailable. Saving an API key will use Glint's private auth.json file.";
const KEYRING_UNAVAILABLE_DELETE_ERROR: &str =
    "The system credential store is unavailable. Provider configuration was not changed.";

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CredentialId(String);

impl CredentialId {
    pub fn builtin(provider_id: &str) -> Self {
        Self(format!("builtin:{provider_id}"))
    }

    pub fn custom(provider_name: &str) -> Self {
        Self(format!("custom:{provider_name}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub trait CredentialStore: Send + Sync {
    fn get(&self, id: &CredentialId) -> Result<Option<String>>;
    fn set(&self, id: &CredentialId, api_key: &str) -> Result<()>;
    fn delete(&self, id: &CredentialId) -> Result<()>;

    fn status(&self) -> CredentialStoreStatus {
        CredentialStoreStatus::Ready
    }
}

/// The availability state consumers should show before asking a user to save a credential.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialStoreStatus {
    Ready,
    KeyringUnavailable { diagnostic: String },
}

#[derive(Default, Deserialize, Serialize)]
struct AuthFile {
    #[serde(default)]
    credentials: BTreeMap<String, String>,
}

#[derive(Clone)]
pub struct FileCredentialStore {
    root: PathBuf,
    path: PathBuf,
    writer: Arc<dyn AtomicFileWriter>,
}

impl fmt::Debug for FileCredentialStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileCredentialStore")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl FileCredentialStore {
    pub fn new(path: PathBuf) -> Self {
        Self::with_writer(path, Arc::new(FsAtomicFileWriter))
    }

    fn with_writer(path: PathBuf, writer: Arc<dyn AtomicFileWriter>) -> Self {
        let root = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Self { root, path, writer }
    }

    fn load(&self) -> Result<AuthFile> {
        create_private_directory(&self.root)?;
        match fs::metadata(&self.path) {
            Ok(_) => restrict_auth_permissions(&self.path)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to inspect {}", self.path.display()));
            }
        }
        match fs::read_to_string(&self.path) {
            Ok(contents) => serde_json::from_str(&contents)
                .with_context(|| format!("failed to parse {}", self.path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(AuthFile::default()),
            Err(error) => {
                Err(error).with_context(|| format!("failed to read {}", self.path.display()))
            }
        }
    }

    fn save(&self, auth: &AuthFile) -> Result<()> {
        create_private_directory(&self.root)?;
        let bytes = serde_json::to_vec(auth)
            .with_context(|| format!("failed to serialize {}", self.path.display()))?;
        self.writer.write(&self.path, &bytes, 0o600)?;
        restrict_auth_permissions(&self.path)
    }
}

#[cfg(unix)]
fn restrict_auth_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to restrict permissions on {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_auth_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

impl CredentialStore for FileCredentialStore {
    fn get(&self, id: &CredentialId) -> Result<Option<String>> {
        Ok(self.load()?.credentials.get(id.as_str()).cloned())
    }

    fn set(&self, id: &CredentialId, api_key: &str) -> Result<()> {
        let mut auth = self.load()?;
        auth.credentials
            .insert(id.as_str().to_owned(), api_key.to_owned());
        self.save(&auth)
    }

    fn delete(&self, id: &CredentialId) -> Result<()> {
        let mut auth = self.load()?;
        if auth.credentials.remove(id.as_str()).is_some() {
            self.save(&auth)?;
        }
        Ok(())
    }
}

pub struct KeyringCredentialStore;

impl fmt::Debug for KeyringCredentialStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("KeyringCredentialStore")
    }
}

impl KeyringCredentialStore {
    fn entry(id: &CredentialId) -> Result<keyring::Entry> {
        keyring::Entry::new(KEYRING_SERVICE, id.as_str())
            .map_err(|_| anyhow!("system credential store failed for {}", id.as_str()))
    }
}

impl CredentialStore for KeyringCredentialStore {
    fn get(&self, id: &CredentialId) -> Result<Option<String>> {
        match Self::entry(id)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(anyhow!(
                "system credential store failed for {}",
                id.as_str()
            )),
        }
    }

    fn set(&self, id: &CredentialId, api_key: &str) -> Result<()> {
        Self::entry(id)?
            .set_password(api_key)
            .map_err(|_| anyhow!("system credential store failed for {}", id.as_str()))
    }

    fn delete(&self, id: &CredentialId) -> Result<()> {
        match Self::entry(id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(anyhow!(
                "system credential store failed for {}",
                id.as_str()
            )),
        }
    }
}

struct DeferredFileCredentialStore {
    state: Mutex<DeferredState>,
}

enum DeferredState {
    Unavailable(FileCredentialStore),
    Ready(FileCredentialStore),
}

impl CredentialStore for DeferredFileCredentialStore {
    fn get(&self, id: &CredentialId) -> Result<Option<String>> {
        match &*self
            .state
            .lock()
            .expect("credential backend mutex poisoned")
        {
            DeferredState::Unavailable(_) => Ok(None),
            DeferredState::Ready(store) => store.get(id),
        }
    }

    fn set(&self, id: &CredentialId, api_key: &str) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .expect("credential backend mutex poisoned");
        match &*state {
            DeferredState::Unavailable(store) => {
                store.set(id, api_key)?;
                *state = DeferredState::Ready(store.clone());
                Ok(())
            }
            DeferredState::Ready(store) => store.set(id, api_key),
        }
    }

    fn delete(&self, id: &CredentialId) -> Result<()> {
        match &*self
            .state
            .lock()
            .expect("credential backend mutex poisoned")
        {
            DeferredState::Unavailable(_) => Err(anyhow!(KEYRING_UNAVAILABLE_DELETE_ERROR)),
            DeferredState::Ready(store) => store.delete(id),
        }
    }

    fn status(&self) -> CredentialStoreStatus {
        match &*self
            .state
            .lock()
            .expect("credential backend mutex poisoned")
        {
            DeferredState::Unavailable(_) => CredentialStoreStatus::KeyringUnavailable {
                diagnostic: KEYRING_UNAVAILABLE_DIAGNOSTIC.to_owned(),
            },
            DeferredState::Ready(_) => CredentialStoreStatus::Ready,
        }
    }
}

trait CredentialBackendFactory {
    fn open_keyring(&self) -> Result<Box<dyn CredentialStore>>;
    fn file_store(&self, auth_path: PathBuf) -> FileCredentialStore;
}

struct DefaultCredentialBackendFactory;

impl CredentialBackendFactory for DefaultCredentialBackendFactory {
    fn open_keyring(&self) -> Result<Box<dyn CredentialStore>> {
        Ok(Box::new(KeyringCredentialStore))
    }

    fn file_store(&self, auth_path: PathBuf) -> FileCredentialStore {
        FileCredentialStore::new(auth_path)
    }
}

pub fn open_credential_store(
    paths: &GlintPaths,
    has_configured_providers: bool,
) -> Result<Box<dyn CredentialStore>> {
    open_with_factory(
        paths,
        has_configured_providers,
        &DefaultCredentialBackendFactory,
    )
}

fn open_with_factory(
    paths: &GlintPaths,
    has_configured_providers: bool,
    factory: &dyn CredentialBackendFactory,
) -> Result<Box<dyn CredentialStore>> {
    if paths.auth().exists() {
        return Ok(Box::new(factory.file_store(paths.auth())));
    }

    match factory.open_keyring().and_then(|store| {
        store
            .get(&CredentialId(KEYRING_PROBE_ID.to_owned()))
            .map(|_| store)
    }) {
        Ok(store) => Ok(store),
        Err(_) if !has_configured_providers => Ok(Box::new(factory.file_store(paths.auth()))),
        Err(_) => Ok(Box::new(DeferredFileCredentialStore {
            state: Mutex::new(DeferredState::Unavailable(factory.file_store(paths.auth()))),
        })),
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use anyhow::{Result, bail};

    use super::*;
    use crate::{
        config::{UserConfig, UserLlmConfig},
        configuration::{ConfigurationManager, ConfigurationMutationErrorKind},
        paths::GlintPaths,
        persistence::UserConfigStore,
        persistence::{AtomicFileWriter, FsAtomicFileWriter},
        provider_catalog::ProviderCatalog,
    };

    #[test]
    fn credential_ids_distinguish_builtin_and_custom_providers() {
        assert_eq!(
            CredentialId::builtin("deepseek").as_str(),
            "builtin:deepseek"
        );
        assert_eq!(
            CredentialId::custom("Team Gateway").as_str(),
            "custom:Team Gateway"
        );
    }

    #[test]
    fn file_store_round_trips_without_debug_secret_exposure() {
        let paths = GlintPaths::from_home(temp_root("file-credentials"));
        let store = FileCredentialStore::new(paths.auth());
        let id = CredentialId::builtin("deepseek");

        store.set(&id, "secret-value").unwrap();

        assert_eq!(store.get(&id).unwrap().as_deref(), Some("secret-value"));
        assert!(!format!("{store:?}").contains("secret-value"));
    }

    #[cfg(unix)]
    #[test]
    fn file_store_uses_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let paths = GlintPaths::from_home(temp_root("file-mode"));
        let store = FileCredentialStore::new(paths.auth());
        store
            .set(&CredentialId::builtin("deepseek"), "secret")
            .unwrap();

        assert_eq!(
            fs::metadata(paths.root()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(paths.auth()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_store_restricts_an_existing_auth_file_to_private_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let paths = GlintPaths::from_home(temp_root("existing-file-mode"));
        let store = FileCredentialStore::new(paths.auth());
        let id = CredentialId::builtin("deepseek");
        store.set(&id, "first").unwrap();
        fs::set_permissions(paths.auth(), fs::Permissions::from_mode(0o644)).unwrap();

        store.set(&id, "second").unwrap();

        assert_eq!(
            fs::metadata(paths.auth()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn file_store_restricts_auth_file_permissions_before_reading() {
        use std::os::unix::fs::PermissionsExt;

        let paths = GlintPaths::from_home(temp_root("read-file-mode"));
        let store = FileCredentialStore::new(paths.auth());
        let id = CredentialId::builtin("deepseek");
        store.set(&id, "secret").unwrap();
        fs::set_permissions(paths.auth(), fs::Permissions::from_mode(0o644)).unwrap();

        assert_eq!(store.get(&id).unwrap().as_deref(), Some("secret"));

        assert_eq!(
            fs::metadata(paths.auth()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn reading_existing_file_backend_restricts_root_and_auth_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let paths = GlintPaths::from_home(temp_root("read-existing-root-mode"));
        fs::create_dir_all(paths.root()).unwrap();
        fs::write(
            paths.auth(),
            r#"{"credentials":{"builtin:deepseek":"secret"}}"#,
        )
        .unwrap();
        fs::set_permissions(paths.root(), fs::Permissions::from_mode(0o755)).unwrap();
        fs::set_permissions(paths.auth(), fs::Permissions::from_mode(0o644)).unwrap();
        let factory = FakeFactory::unavailable();
        let store = open_with_factory(&paths, true, &factory).unwrap();

        assert_eq!(
            store
                .get(&CredentialId::builtin("deepseek"))
                .unwrap()
                .as_deref(),
            Some("secret")
        );
        assert_eq!(
            fs::metadata(paths.root()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(paths.auth()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn selection_prefers_an_existing_auth_file() {
        let paths = GlintPaths::from_home(temp_root("existing-file"));
        let file = FileCredentialStore::new(paths.auth());
        file.set(&CredentialId::builtin("deepseek"), "file-secret")
            .unwrap();
        let factory = FakeFactory::unavailable();

        let store = open_with_factory(&paths, true, &factory).unwrap();

        assert_eq!(
            store
                .get(&CredentialId::builtin("deepseek"))
                .unwrap()
                .as_deref(),
            Some("file-secret")
        );
    }

    #[test]
    fn selection_uses_reachable_keyring_when_no_auth_file_exists() {
        let paths = GlintPaths::from_home(temp_root("keyring"));
        let factory = FakeFactory::available();

        let store = open_with_factory(&paths, true, &factory).unwrap();

        assert_eq!(store.status(), CredentialStoreStatus::Ready);
        assert_eq!(factory.keyring_opens(), 1);
    }

    #[test]
    fn fresh_unconfigured_install_falls_back_to_file_when_keyring_is_unavailable() {
        let paths = GlintPaths::from_home(temp_root("fresh-file"));
        let factory = FakeFactory::unavailable();

        let store = open_with_factory(&paths, false, &factory).unwrap();

        store
            .set(&CredentialId::builtin("deepseek"), "file-secret")
            .unwrap();
        assert!(paths.auth().exists());
        assert_eq!(store.status(), CredentialStoreStatus::Ready);
    }

    #[test]
    fn configured_install_defers_file_creation_until_first_explicit_set() {
        let paths = GlintPaths::from_home(temp_root("deferred-file"));
        let factory = FakeFactory::unavailable();
        let id = CredentialId::builtin("deepseek");

        let store = open_with_factory(&paths, true, &factory).unwrap();

        assert_eq!(store.get(&id).unwrap(), None);
        assert_eq!(
            store.status(),
            CredentialStoreStatus::KeyringUnavailable {
                diagnostic: "The system credential store is unavailable. Saving an API key will use Glint's private auth.json file.".to_owned(),
            }
        );
        assert!(!paths.auth().exists());

        store.set(&id, "file-secret").unwrap();

        assert_eq!(store.status(), CredentialStoreStatus::Ready);
        assert_eq!(store.get(&id).unwrap().as_deref(), Some("file-secret"));
        assert!(paths.auth().exists());
    }

    #[test]
    fn configured_install_rejects_delete_while_keyring_is_unavailable() {
        let paths = GlintPaths::from_home(temp_root("deferred-delete"));
        let factory = FakeFactory::unavailable();
        let id = CredentialId::builtin("deepseek");
        let store = open_with_factory(&paths, true, &factory).unwrap();

        let error = store.delete(&id).unwrap_err();

        assert_eq!(
            error.to_string(),
            "The system credential store is unavailable. Provider configuration was not changed."
        );
        assert!(!error.to_string().contains(id.as_str()));
        assert!(!paths.auth().exists());
        assert!(matches!(
            store.status(),
            CredentialStoreStatus::KeyringUnavailable { .. }
        ));
    }

    #[test]
    fn deferred_delete_failure_preserves_manager_and_yaml_configuration() {
        let home = temp_root("deferred-manager-delete");
        let paths = GlintPaths::from_home(&home);
        let repository = UserConfigStore::new(paths.clone());
        let mut user = UserConfig::default();
        user.configured_providers.push("deepseek".into());
        user.llm = Some(UserLlmConfig {
            provider: "deepseek".into(),
            model: "deepseek-v4-flash".into(),
            temperature: 0.7,
            max_tokens: 8196,
        });
        repository.save(&user).unwrap();
        let store = open_with_factory(&paths, true, &FakeFactory::unavailable()).unwrap();
        let mut manager = ConfigurationManager::new(
            paths.clone(),
            PathBuf::from("/workspace"),
            ProviderCatalog::embedded().unwrap(),
            Box::new(UserConfigStore::new(paths.clone())),
            store,
        )
        .unwrap();

        let error = manager.delete_provider("deepseek").unwrap_err();

        assert_eq!(
            error.kind(),
            ConfigurationMutationErrorKind::CredentialUnavailable
        );
        assert_eq!(manager.user_config(), &user);
        assert_eq!(repository.load().unwrap(), Some(user));
        assert!(!paths.auth().exists());
        fs::remove_dir_all(home).ok();
    }

    #[test]
    fn file_store_replaces_secrets_and_deletes_idempotently() {
        let paths = GlintPaths::from_home(temp_root("replace-delete"));
        let store = FileCredentialStore::new(paths.auth());
        let id = CredentialId::builtin("deepseek");

        store.set(&id, "first").unwrap();
        store.set(&id, "second").unwrap();
        store.delete(&id).unwrap();
        store.delete(&id).unwrap();

        assert_eq!(store.get(&id).unwrap(), None);
    }

    #[test]
    fn failed_atomic_replace_preserves_existing_auth_file() {
        let paths = GlintPaths::from_home(temp_root("atomic-preserve"));
        let id = CredentialId::builtin("deepseek");
        let original = FileCredentialStore::new(paths.auth());
        original.set(&id, "first").unwrap();
        let store = FileCredentialStore::with_writer(paths.auth(), Arc::new(FailingWriter));

        assert!(store.set(&id, "second").is_err());

        assert_eq!(original.get(&id).unwrap().as_deref(), Some("first"));
    }

    #[derive(Debug)]
    struct FailingWriter;

    impl AtomicFileWriter for FailingWriter {
        fn write(&self, _: &std::path::Path, _: &[u8], _: u32) -> Result<()> {
            bail!("injected atomic write failure")
        }
    }

    #[derive(Clone)]
    enum KeyringAvailability {
        Available,
        Unavailable,
    }

    struct FakeFactory {
        availability: KeyringAvailability,
        keyring_opens: std::sync::atomic::AtomicUsize,
    }

    impl FakeFactory {
        fn available() -> Self {
            Self {
                availability: KeyringAvailability::Available,
                keyring_opens: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn unavailable() -> Self {
            Self {
                availability: KeyringAvailability::Unavailable,
                keyring_opens: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn keyring_opens(&self) -> usize {
            self.keyring_opens
                .load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    impl CredentialBackendFactory for FakeFactory {
        fn open_keyring(&self) -> Result<Box<dyn CredentialStore>> {
            self.keyring_opens
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            match self.availability {
                KeyringAvailability::Available => Ok(Box::new(MemoryCredentialStore::default())),
                KeyringAvailability::Unavailable => bail!("keyring unavailable"),
            }
        }

        fn file_store(&self, auth_path: std::path::PathBuf) -> FileCredentialStore {
            FileCredentialStore::with_writer(auth_path, Arc::new(FsAtomicFileWriter))
        }
    }

    #[derive(Default)]
    struct MemoryCredentialStore(std::sync::Mutex<std::collections::BTreeMap<String, String>>);

    impl CredentialStore for MemoryCredentialStore {
        fn get(&self, id: &CredentialId) -> Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(id.as_str()).cloned())
        }

        fn set(&self, id: &CredentialId, api_key: &str) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .insert(id.as_str().to_owned(), api_key.to_owned());
            Ok(())
        }

        fn delete(&self, id: &CredentialId) -> Result<()> {
            self.0.lock().unwrap().remove(id.as_str());
            Ok(())
        }
    }

    fn temp_root(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("glint-credentials-{name}-{}", uuid::Uuid::new_v4()))
    }
}
