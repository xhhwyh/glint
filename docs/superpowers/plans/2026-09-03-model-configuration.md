# Model Configuration and First-run Setup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace source-directory configuration and manual initialization with a fixed `~/.glint` user configuration, protected API-key storage, embedded provider presets, automatic first-run setup, and unified `/model` management.

**Architecture:** Parse an embedded provider catalog separately from a small serializable user configuration, then combine both with a credential-store abstraction in `ConfigurationManager`. A pure setup state machine drives both first-run and in-session provider management; Ratatui rendering remains side-effect free, while configuration and key writes are explicit effects handled by the manager.

**Tech Stack:** Rust 2024, serde/serde_yaml, anyhow, Ratatui, Crossterm, clap, keyring 4.2 `v1`, cargo-dist 0.32.

**Spec:** `docs/superpowers/specs/2026-09-03-model-configuration-design.md`

## Global Constraints

- All global Glint state introduced or selected by this feature lives below `~/.glint`; the process working directory remains the workspace used by coding tools.
- `~/.glint/config.yaml` is the only user-editable global settings file.
- Built-in provider URLs, model lists, prices, context limits, and prompt-cache settings are compiled into the executable and never copied into the user file.
- API keys never enter YAML, command-line arguments, logs, errors, transcripts, or `Debug` output.
- Built-in setup asks only for an API key and enables every model in that provider's embedded catalog.
- Custom provider names are case-insensitively unique and cannot collide with built-in IDs or display names.
- Saving setup performs local validation only and sends no provider network request.
- Remove `glint init`, `--config`, `GLINT_CONFIG`, project config discovery, and the current-directory legacy fallback.
- Preserve the existing event boundary: state changes flow through update functions and rendering has no filesystem or keyring side effects.
- Preserve existing dist targets and installer behavior for macOS, Linux, and WSL; native Windows remains unsupported.
- Keep project-specific Bash permission grants workspace-local; they are authorization state rather than the global model/extension configuration in scope here.

## File Structure

**Create:**

- `assets/providers.yaml` — shipped provider catalog, compiled into the binary.
- `src/provider_catalog.rs` — catalog schema, parsing, validation, lookup, and runtime model metadata.
- `src/paths.rs` — injectable `~/.glint` path calculation.
- `src/persistence.rs` — reusable atomic private-file writer for configuration and fallback credentials.
- `src/credentials.rs` — credential IDs, keyring backend, protected JSON fallback, and backend selection.
- `src/configuration.rs` — available-model calculation and transactional provider/model mutations.
- `src/setup/mod.rs` — shared setup effects and persistence application.
- `src/setup/state.rs` — pure setup navigation, form state, and key handling.
- `src/ui/setup.rs` — full-screen setup renderer.

**Modify:**

- `Cargo.toml`, `Cargo.lock` — add the keyring dependency.
- `src/main.rs` — module registration, fixed-path bootstrap, first-run loop, and chat handoff.
- `src/cli.rs` — retain only normal run/help/version parsing.
- `src/config.rs` — replace mixed file schema with runtime-only model/extension configuration.
- `src/app.rs` — own `ConfigurationManager`, route setup input, persist model selection, and recover after provider deletion.
- `src/ui/mod.rs` — route full-screen setup rendering before chat rendering.
- `src/ui/model_picker.rs` — configured-only providers plus `Add model`.
- `src/services/mcp/config.rs` — keep fixed config persistence compatible with the new root schema.
- `src/plugins/mod.rs` — resolve user-configured relative plugin paths from the global state root.
- `src/runtime/mod.rs`, `src/services/mcp/manager.rs` — separate workspace execution roots from global MCP state/config roots where necessary.
- `src/transcript.rs` — accept the explicit global session root rather than rediscovering `HOME`.
- `tests/cli.rs` — assert removed legacy CLI and non-interactive first-run behavior.
- `README.md`, `AGENTS.md`, `EXTENSIONS.md` — document the fixed layout, setup UI, credential behavior, and custom schema.

**Remove:**

- `config.yaml` — former source-directory runtime configuration and duplicate provider metadata.
- `config.example.yaml` — manual starter configuration is no longer part of startup or packaging.

---

### Task 1: Embed and validate the built-in provider catalog

**Files:**

- Create: `assets/providers.yaml`
- Create: `src/provider_catalog.rs`
- Modify: `src/main.rs`
- Test: `src/provider_catalog.rs`

**Interfaces:**

- Produces: `ProviderCatalog::embedded() -> anyhow::Result<ProviderCatalog>`
- Produces: `ProviderCatalog::builtin(&self, id: &str) -> Option<&ProviderDefinition>`
- Produces: `ProviderDefinition`, `ModelDefinition`, `ModelMetadata`, and `PromptCacheConfig`
- Consumes: the exact provider metadata currently under `llm.providers` in the checked-in `config.yaml`

- [ ] **Step 1: Write failing embedded-catalog tests**

Add a `#[cfg(test)]` module to the new module and register `mod provider_catalog;` in `src/main.rs`:

```rust
#[test]
fn embedded_catalog_contains_every_shipped_provider() {
    let catalog = ProviderCatalog::embedded().expect("embedded catalog");
    let ids = catalog
        .providers()
        .iter()
        .map(|provider| provider.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(
        ids,
        ["deepseek", "volcengine", "zhipu", "kimi", "dashscope", "openrouter"]
    );
}

#[test]
fn every_builtin_default_is_a_declared_model() {
    let catalog = ProviderCatalog::embedded().expect("embedded catalog");

    for provider in catalog.providers() {
        assert!(
            provider
                .models
                .iter()
                .any(|model| model.name == provider.default_model),
            "{} has an invalid default",
            provider.id
        );
    }
}

#[test]
fn catalog_rejects_case_insensitive_provider_collisions() {
    let yaml = r#"
providers:
  - id: demo
    name: Demo
    base_url: https://one.example/v1
    default_model: one
    models: [{ name: one }]
  - id: DEMO
    name: Other
    base_url: https://two.example/v1
    default_model: two
    models: [{ name: two }]
"#;

    let error = ProviderCatalog::parse(yaml).unwrap_err();
    assert!(format!("{error:#}").contains("duplicate provider"));
}
```

- [ ] **Step 2: Run the tests to verify RED**

Run: `cargo test provider_catalog::tests -- --nocapture`

Expected: compilation fails because `provider_catalog` types and methods do not exist.

- [ ] **Step 3: Add the catalog schema and validation**

Implement these public shapes in `src/provider_catalog.rs`:

```rust
const EMBEDDED_PROVIDERS: &str = include_str!("../assets/providers.yaml");

#[derive(Clone, Debug, Deserialize)]
struct CatalogFile {
    providers: Vec<ProviderDefinition>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ProviderDefinition {
    pub id: String,
    pub name: String,
    pub description: String,
    pub base_url: String,
    pub default_model: String,
    #[serde(default)]
    pub unit: String,
    pub models: Vec<ModelDefinition>,
    #[serde(default)]
    pub prompt_cache: PromptCacheConfig,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ModelDefinition {
    pub name: String,
    #[serde(flatten)]
    pub metadata: ModelMetadata,
}

#[derive(Clone, Debug)]
pub struct ProviderCatalog {
    providers: Vec<ProviderDefinition>,
}

impl ProviderCatalog {
    pub fn embedded() -> Result<Self> {
        Self::parse(EMBEDDED_PROVIDERS)
    }

    fn parse(yaml: &str) -> Result<Self> {
        let file: CatalogFile = serde_yaml::from_str(yaml)?;
        validate_catalog(&file.providers)?;
        Ok(Self { providers: file.providers })
    }

    pub fn providers(&self) -> &[ProviderDefinition] {
        &self.providers
    }

    pub fn builtin(&self, id: &str) -> Option<&ProviderDefinition> {
        self.providers.iter().find(|provider| provider.id == id)
    }
}
```

Move the existing display metadata fields (`positioning`, price fields, `context`, and `max_tokens`) and prompt-cache representation out of `src/config.rs` into `ModelMetadata` and `PromptCacheConfig`. Preserve numeric YAML values as display strings and numeric context values as `Option<u64>` for runtime limits.

Populate `assets/providers.yaml` with all six existing providers and their complete current model metadata. Add `default_model` equal to the first listed model for each provider. Trim trailing slashes only when producing a runtime URL, not while parsing the source asset.

- [ ] **Step 4: Run catalog and existing configuration tests**

Run: `cargo test provider_catalog::tests config::tests -- --nocapture`

Expected: PASS. Existing runtime configuration still loads through the legacy path at this task boundary.

- [ ] **Step 5: Commit the catalog boundary**

```bash
git add assets/providers.yaml src/provider_catalog.rs src/config.rs src/main.rs
git commit -m "refactor(config): embed provider catalog" \
  -m "- move stable provider and model metadata into a compiled asset" \
  -m "- validate provider identities, defaults, endpoints, and model lists"
```

### Task 2: Add fixed Glint paths and the concise user schema

**Files:**

- Create: `src/paths.rs`
- Create: `src/persistence.rs`
- Modify: `src/config.rs`
- Modify: `src/main.rs`
- Test: `src/paths.rs`
- Test: `src/config.rs`

**Interfaces:**

- Produces: `GlintPaths::discover() -> anyhow::Result<GlintPaths>`
- Produces: `GlintPaths::from_home(home: impl Into<PathBuf>) -> GlintPaths`
- Produces: `UserConfigStore::load(&self) -> anyhow::Result<Option<UserConfig>>`
- Produces: `UserConfigStore::save(&self, config: &UserConfig) -> anyhow::Result<()>`
- Produces: `UserConfigRepository::{path,load,save}`, implemented by `UserConfigStore` and test doubles
- Produces: `AtomicFileWriter::write(&self, path: &Path, bytes: &[u8], mode: u32) -> anyhow::Result<()>`
- Produces: `UserConfig`, `UserLlmConfig`, and `CustomProviderConfig`
- Consumes: `McpConfig`, `PluginsConfig`, and `LspConfig` by parsing optional raw YAML sections when runtime configuration is built

- [ ] **Step 1: Write failing path and serialization tests**

```rust
#[test]
fn fixed_paths_are_all_below_dot_glint() {
    let paths = GlintPaths::from_home("/users/alice");

    assert_eq!(paths.root(), Path::new("/users/alice/.glint"));
    assert_eq!(paths.config(), Path::new("/users/alice/.glint/config.yaml"));
    assert_eq!(paths.auth(), Path::new("/users/alice/.glint/auth.json"));
    assert_eq!(paths.plugins(), Path::new("/users/alice/.glint/plugins"));
    assert_eq!(paths.mcp(), Path::new("/users/alice/.glint/mcp"));
    assert_eq!(paths.sessions(), Path::new("/users/alice/.glint/sessions"));
}

#[test]
fn user_config_omits_empty_optional_sections() {
    let config = UserConfig {
        version: 1,
        llm: Some(UserLlmConfig {
            provider: "deepseek".into(),
            model: "deepseek-v4-flash".into(),
            temperature: 0.7,
            max_tokens: 8196,
        }),
        ..UserConfig::default()
    };

    let yaml = serde_yaml::to_string(&config).unwrap();
    assert!(!yaml.contains("configured_providers:"));
    assert!(!yaml.contains("custom_providers:"));
    assert!(!yaml.contains("mcp:"));
    assert!(!yaml.contains("plugins:"));
    assert!(!yaml.contains("lsp:"));
}

#[test]
fn malformed_user_config_is_reported_without_replacement() {
    let root = temp_root("malformed-user-config");
    let paths = GlintPaths::from_home(&root);
    fs::create_dir_all(paths.root()).unwrap();
    fs::write(paths.config(), "llm: [broken\n").unwrap();
    let store = UserConfigStore::new(paths.clone());

    let error = store.load().unwrap_err();

    assert!(format!("{error:#}").contains(&paths.config().display().to_string()));
    assert_eq!(fs::read_to_string(paths.config()).unwrap(), "llm: [broken\n");
}
```

- [ ] **Step 2: Run the new tests to verify RED**

Run: `cargo test paths::tests config::tests::user_config -- --nocapture`

Expected: compilation fails because `GlintPaths`, `UserConfig`, and `UserConfigStore` are absent.

- [ ] **Step 3: Implement fixed paths and the new file schema alongside the runtime schema**

Use injectable home paths so unit tests never mutate the developer's real `~/.glint`:

```rust
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
        Self { root: home.into().join(".glint") }
    }

    pub fn root(&self) -> &Path { &self.root }
    pub fn config(&self) -> PathBuf { self.root.join("config.yaml") }
    pub fn auth(&self) -> PathBuf { self.root.join("auth.json") }
    pub fn plugins(&self) -> PathBuf { self.root.join("plugins") }
    pub fn mcp(&self) -> PathBuf { self.root.join("mcp") }
    pub fn sessions(&self) -> PathBuf { self.root.join("sessions") }
}
```

Represent extension sections as optional `serde_yaml::Value` fields so a model switch can round-trip them without expanding every nested default:

```rust
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UserConfig {
    #[serde(default = "schema_version")]
    pub version: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub llm: Option<UserLlmConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub configured_providers: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_providers: BTreeMap<String, CustomProviderConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<serde_yaml::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugins: Option<serde_yaml::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lsp: Option<serde_yaml::Value>,
}

impl Default for UserConfig {
    fn default() -> Self {
        Self {
            version: schema_version(),
            llm: None,
            configured_providers: Vec::new(),
            custom_providers: BTreeMap::new(),
            mcp: None,
            plugins: None,
            lsp: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UserLlmConfig {
    pub provider: String,
    pub model: String,
    pub temperature: f32,
    pub max_tokens: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CustomProviderConfig {
    pub base_url: String,
    pub models: Vec<String>,
}
```

`UserConfigStore::save` must create `~/.glint` with private Unix permissions, serialize to a sibling temporary file, flush it, copy existing permissions when replacing a file, and rename it over `config.yaml`. On a first write, set mode `0600` on Unix.

Put the filesystem operation behind this small reusable boundary:

```rust
pub trait AtomicFileWriter: Send + Sync {
    fn write(&self, path: &Path, bytes: &[u8], unix_mode: u32) -> Result<()>;
}

pub struct FsAtomicFileWriter;

pub trait UserConfigRepository: Send {
    fn path(&self) -> &Path;
    fn load(&self) -> Result<Option<UserConfig>>;
    fn save(&self, config: &UserConfig) -> Result<()>;
}
```

`FsAtomicFileWriter` captures a rename error, attempts to remove only its own sibling temporary file, and then returns the captured error. `UserConfigStore` receives an `Arc<dyn AtomicFileWriter>` in tests and uses `FsAtomicFileWriter` in production.

- [ ] **Step 4: Test round trips and atomic replacement**

Add tests that load a file containing populated `mcp`, `plugins`, and `lsp`, change only `llm.model`, save, and assert the three extension values are structurally unchanged. Also write an existing valid file, force the repository's injected atomic-writer test double to fail before rename, and assert the original bytes remain unchanged.

Run: `cargo test paths::tests config::tests::user_config -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit the user configuration store**

```bash
git add src/paths.rs src/persistence.rs src/config.rs src/main.rs
git commit -m "feat(config): add fixed user configuration" \
  -m "- resolve the global state root exclusively under ~/.glint" \
  -m "- atomically persist concise user settings and extension sections"
```

### Task 3: Add protected credential backends

**Files:**

- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Create: `src/credentials.rs`
- Modify: `src/main.rs`
- Test: `src/credentials.rs`

**Interfaces:**

- Produces: `CredentialId::builtin(provider_id: &str) -> CredentialId`
- Produces: `CredentialId::custom(provider_name: &str) -> CredentialId`
- Produces: `CredentialStore::{get,set,delete}`
- Produces: `open_credential_store(paths: &GlintPaths, has_configured_providers: bool) -> anyhow::Result<Box<dyn CredentialStore>>`
- Consumes: `GlintPaths::auth()` and credential IDs supplied by `ConfigurationManager`
- Consumes: `AtomicFileWriter` for protected fallback-file replacement

- [ ] **Step 1: Add failing credential contract tests with an in-memory implementation**

```rust
pub trait CredentialStore: Send + Sync {
    fn get(&self, id: &CredentialId) -> Result<Option<String>>;
    fn set(&self, id: &CredentialId, api_key: &str) -> Result<()>;
    fn delete(&self, id: &CredentialId) -> Result<()>;
}

#[test]
fn credential_ids_distinguish_builtin_and_custom_providers() {
    assert_eq!(CredentialId::builtin("deepseek").as_str(), "builtin:deepseek");
    assert_eq!(CredentialId::custom("Team Gateway").as_str(), "custom:Team Gateway");
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

    assert_eq!(fs::metadata(paths.root()).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(paths.auth()).unwrap().permissions().mode() & 0o777, 0o600);
}
```

- [ ] **Step 2: Run credential tests to verify RED**

Run: `cargo test credentials::tests -- --nocapture`

Expected: compilation fails because the credential module is not implemented.

- [ ] **Step 3: Implement file and keyring stores**

Add the verified keyring v1 compatibility API:

```toml
keyring = { version = "4.2.0", default-features = false, features = ["v1"] }
```

Implement `KeyringCredentialStore` with service name `glint` and `keyring::Entry::new("glint", id.as_str())`. Map `keyring::Error::NoEntry` to `Ok(None)` and preserve every other error with provider identity only, never the secret. Use `set_password`, `get_password`, and `delete_credential`; treat deletion of a missing entry as success.

Implement `FileCredentialStore` with this private schema and atomic file replacement:

```rust
#[derive(Default, Deserialize, Serialize)]
struct AuthFile {
    #[serde(default)]
    credentials: BTreeMap<String, String>,
}
```

Backend selection rules:

1. If `auth.json` exists, use it.
2. Otherwise probe the keyring with a reserved `glint-probe` entry; a missing entry proves the store is reachable.
3. On a fresh configuration with no configured providers, fall back to `auth.json` if the keyring is unavailable.
4. With configured providers, report keyring unavailability instead of silently creating a second store.

Use a private `CredentialBackendFactory` trait in tests to simulate available, missing, and unavailable keyring results without touching the developer's keyring.

- [ ] **Step 4: Exercise backend selection and deletion**

Add tests for all four selection rules, replacement of an existing secret, idempotent deletion, and preservation of the previous `auth.json` when an injected atomic write fails.

Run: `cargo test credentials::tests -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Commit credential storage**

```bash
git add Cargo.toml Cargo.lock src/credentials.rs src/main.rs
git commit -m "feat(auth): store provider credentials securely" \
  -m "- prefer the operating-system keyring for provider API keys" \
  -m "- add a private atomic auth.json fallback under ~/.glint"
```

### Task 4: Build the unified configuration manager

**Files:**

- Create: `src/configuration.rs`
- Modify: `src/config.rs`
- Modify: `src/main.rs`
- Modify: `src/app.rs`
- Modify: `src/agent/compact.rs`
- Modify: `src/http_proxy_tests.rs`
- Modify: `src/query/mod.rs`
- Test: `src/configuration.rs`
- Test: `src/config.rs`

**Interfaces:**

- Consumes: `ProviderCatalog`, `UserConfigStore`, and `CredentialStore`
- Produces: `ConfigurationManager::discover(paths: GlintPaths, workspace: &Path) -> anyhow::Result<ConfigurationManager>`
- Produces: injectable `ConfigurationManager::new(paths, catalog, repository, credentials) -> anyhow::Result<ConfigurationManager>` for tests
- Produces: `ConfigurationManager::available_providers(&self) -> anyhow::Result<Vec<AvailableProvider>>`
- Produces: `ConfigurationManager::provider_statuses(&self) -> anyhow::Result<Vec<ProviderStatus>>`
- Produces: `ConfigurationManager::build_runtime(&self, workspace: &Path) -> anyhow::Result<Config>`
- Produces: `ConfigurationManager::{save_builtin,save_custom,delete_provider,select_model}`
- Produces: `ConfigurationManager::{paths,user_config,repair_selection}` accessors/repair operation
- Produces: `AvailableProvider` and `AvailableModel`, consumed by setup and `/model`

- [ ] **Step 1: Write failing availability and selection tests**

Use `MemoryCredentialStore` and `MemoryUserConfigStore` test doubles implementing the same interfaces as production stores:

```rust
#[test]
fn builtin_credential_exposes_every_catalog_model() {
    let mut fixture = ManagerFixture::new();
    fixture.user.configured_providers.push("deepseek".into());
    fixture.credentials.insert("builtin:deepseek", "key");

    let manager = fixture.manager();
    let providers = manager.available_providers().unwrap();

    assert_eq!(providers.len(), 1);
    assert_eq!(providers[0].id, "deepseek");
    assert_eq!(
        providers[0].models.iter().map(|model| model.name.as_str()).collect::<Vec<_>>(),
        ["deepseek-v4-flash", "deepseek-v4-pro"]
    );
}

#[test]
fn custom_provider_requires_a_credential_and_model() {
    let mut fixture = ManagerFixture::new();
    fixture.user.custom_providers.insert(
        "Team Gateway".into(),
        CustomProviderConfig {
            base_url: "https://llm.example/v1".into(),
            models: vec!["code-large".into()],
        },
    );

    assert!(fixture.manager().available_providers().unwrap().is_empty());
}

#[test]
fn invalid_current_model_is_repaired_to_builtin_default() {
    let mut fixture = ManagerFixture::new();
    fixture.enable_builtin("deepseek", "key");
    fixture.user.llm = Some(UserLlmConfig {
        provider: "deepseek".into(),
        model: "removed-model".into(),
        temperature: 0.7,
        max_tokens: 8196,
    });

    let mut manager = fixture.manager();
    manager.repair_selection().unwrap();

    assert_eq!(manager.user_config().llm.as_ref().unwrap().model, "deepseek-v4-flash");
}
```

- [ ] **Step 2: Run manager tests to verify RED**

Run: `cargo test configuration::tests -- --nocapture`

Expected: compilation fails because the manager and available-provider types are absent.

- [ ] **Step 3: Implement manager loading and runtime conversion**

Define stable picker/runtime types:

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct AvailableProvider {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub base_url: String,
    pub models: Vec<AvailableModel>,
    pub prompt_cache: PromptCacheConfig,
    pub custom: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AvailableModel {
    pub name: String,
    pub metadata: ModelMetadata,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderStatus {
    pub id: String,
    pub display_name: String,
    pub builtin: bool,
    pub configured: bool,
    pub needs_credential: bool,
    pub model_count: usize,
}
```

`available_providers` must follow catalog order for built-ins, sort custom providers case-insensitively by display name, and preserve model order. It skips a provider when its credential is missing, while a separate `provider_statuses` view marks that provider as needing repair for setup.

Store `Box<dyn UserConfigRepository>` and `Box<dyn CredentialStore>` in the manager. `discover` constructs the embedded catalog and production stores; `new` receives all three dependencies directly. Expose `paths() -> &GlintPaths`, `user_config() -> &UserConfig`, and `repair_selection() -> Result<()>` with the exact names used by later tasks.

Replace `LlmProviderConfig.api_key_env` with `credential_id: CredentialId`. Keep the resolved current key in `LlmConfig.api_key` because the background request path already clones `LlmConfig`; resolve it through the credential store only when constructing or switching runtime configuration.

Parse optional raw `mcp`, `plugins`, and `lsp` values into their existing runtime types in `build_runtime`. Continue applying plugin contributions after parsing, but use the global config root for user-relative plugin paths.

- [ ] **Step 4: Implement transactional mutations**

Use these signatures:

```rust
impl ConfigurationManager {
    pub fn save_builtin(&mut self, provider_id: &str, api_key: Option<&str>) -> Result<()>;

    pub fn save_custom(
        &mut self,
        name: &str,
        base_url: &str,
        api_key: Option<&str>,
        models: Vec<String>,
    ) -> Result<()>;

    pub fn delete_provider(&mut self, provider_id: &str) -> Result<()>;

    pub fn select_model(&mut self, provider_id: &str, model: &str) -> Result<LlmConfig>;
}
```

Stage and validate a cloned `UserConfig`; snapshot the old credential; write the secret; atomically save YAML; then replace the manager's in-memory user config. On YAML failure, restore or delete the staged credential. For deletion, retain enough state to restore both records when either operation fails. Do not update the current `LlmConfig` until the method returns success.

When the first provider is saved, select its catalog default or first custom model with temperature `0.7` and max tokens `8196`. When the active model disappears, call `repair_selection`; when the last model disappears, set `user.llm` to `None`.

- [ ] **Step 5: Replace legacy runtime loading and environment-key resolution**

Delete `ConfigPathInput`, `InitConfigPathInput`, `resolve_config_path`, `resolve_init_config_path`, `init_config`, the old `FileLlmConfig.providers`, and all `api_key_env` resolution. Change runtime construction to accept the selected `AvailableProvider` and credential supplied by `ConfigurationManager`.

Update explicit `LlmConfig` fixtures in `src/app.rs`, `src/agent/compact.rs`, `src/http_proxy_tests.rs`, and `src/query/mod.rs` to use `CredentialId` or direct resolved runtime values without environment-variable callbacks.

- [ ] **Step 6: Verify rollback, ordering, and runtime compatibility**

Add tests for case-insensitive custom-name collision, built-in collision, URL schemes other than HTTP/HTTPS, blank/duplicate models, missing credentials, deterministic ordering, credential rollback after YAML failure, current-model persistence, and last-provider deletion.

Run: `cargo test configuration::tests config::tests agent:: query:: http_proxy_tests -- --nocapture`

Expected: PASS.

- [ ] **Step 7: Commit the unified configuration manager**

```bash
git add src/configuration.rs src/config.rs src/main.rs src/app.rs src/agent/compact.rs src/http_proxy_tests.rs src/query/mod.rs
git commit -m "refactor(config): unify runtime model resolution" \
  -m "- merge catalog, user settings, and protected credentials in one manager" \
  -m "- persist provider mutations and model selection transactionally"
```

### Task 5: Implement the shared setup state machine

**Files:**

- Create: `src/setup/mod.rs`
- Create: `src/setup/state.rs`
- Modify: `src/main.rs`
- Test: `src/setup/state.rs`
- Test: `src/setup/mod.rs`

**Interfaces:**

- Consumes: `ProviderCatalog`, `ConfigurationManager`, `KeyAction`, and `InputState`
- Produces: `SetupState::welcome(...)` and `SetupState::provider_list(...)`
- Produces: `SetupState::{builtin_form_mut,custom_form_mut}` for renderer/controller tests
- Produces: `SetupState::update(&mut self, KeyAction) -> Option<SetupEffect>`
- Produces: `apply_setup_effect(manager, state, effect) -> anyhow::Result<Option<SetupOutcome>>`
- Produces: `SetupOutcome::{StartGlint, Exit}` for the first-run loop and chat transition

- [ ] **Step 1: Write failing navigation and dynamic-row tests**

```rust
#[test]
fn welcome_submit_opens_provider_list() {
    let catalog = test_catalog();
    let mut state = SetupState::welcome(&catalog);

    let effect = state.update(KeyAction::Submit);

    assert_eq!(effect, None);
    assert!(matches!(state.screen, SetupScreen::Providers(_)));
}

#[test]
fn custom_form_starts_with_one_unlabelled_model_row() {
    let form = CustomProviderForm::new();

    assert_eq!(form.models.len(), 1);
    assert_eq!(form.models[0].value(), "");
}

#[test]
fn add_and_delete_model_rows_preserve_one_row() {
    let mut form = CustomProviderForm::new();
    form.add_model_row();
    form.models[0].set("first");
    form.models[1].set("second");

    form.delete_model_row(0);
    assert_eq!(form.models.len(), 1);
    assert_eq!(form.models[0].value(), "second");

    form.delete_model_row(0);
    assert_eq!(form.models.len(), 1);
    assert_eq!(form.models[0].value(), "");
}

#[test]
fn cancel_custom_form_emits_no_persistence_effect() {
    let mut state = SetupState::custom_provider(test_catalog());

    let effect = state.update(KeyAction::Escape);

    assert_eq!(effect, None);
    assert!(matches!(state.screen, SetupScreen::Providers(_)));
}
```

- [ ] **Step 2: Run state tests to verify RED**

Run: `cargo test setup::state::tests setup::tests -- --nocapture`

Expected: compilation fails because setup types do not exist.

- [ ] **Step 3: Implement screens, focus, drafts, and effects**

Use explicit state and effect types:

```rust
#[derive(Clone)]
pub struct SetupState {
    pub screen: SetupScreen,
    pub error: Option<String>,
}

#[derive(Clone)]
pub enum SetupScreen {
    Welcome(WelcomeState),
    Providers(ProviderListState),
    Builtin(BuiltinProviderForm),
    Custom(CustomProviderForm),
    ConfirmDelete(DeleteProviderState),
}

#[derive(Clone, PartialEq, Eq)]
pub enum SetupEffect {
    SaveBuiltin { provider_id: String, api_key: Option<String> },
    SaveCustom {
        name: String,
        base_url: String,
        api_key: Option<String>,
        models: Vec<String>,
    },
    DeleteProvider { provider_id: String },
    StartGlint,
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupOutcome {
    StartGlint,
    Exit,
}
```

Implement manual `Debug` for `SetupEffect`, `SetupState`, `SetupScreen`, `BuiltinProviderForm`, and `CustomProviderForm` that renders API-key fields as `[REDACTED]`. Do not derive `Debug` for any type whose fields could expose the key through nested formatting.

Keep editable text in `InputState`. Track custom form focus as `Name`, `BaseUrl`, `ApiKey`, `Model(usize)`, `AddModel`, `Save`, or `Cancel`. `Tab`, arrows, Enter, Escape, Backspace, Delete, and character input must be deterministic and testable without a terminal.

Provider-list rows include every built-in provider, configured custom providers, `Custom provider`, and—only when a model is available—`Start Glint`. Saving returns to this list and refreshes configured status/model counts from the manager.

- [ ] **Step 4: Implement persistence effect application**

`apply_setup_effect` calls exactly one manager mutation. On success it clears form errors and rebuilds provider-list status. On failure it writes a redacted message to `SetupState.error` and leaves the active draft and focus unchanged. An empty API-key edit for an existing provider passes `None` and retains the current secret; a new provider requires a non-empty key. The manager applies the same rule for built-in and custom providers, so the UI is not the only validation boundary.

Add tests with a failing manager repository proving form values survive save errors, cancellation writes nothing, deletion requires confirmation, and `Start Glint` cannot be emitted with zero available models.

- [ ] **Step 5: Run setup tests**

Run: `cargo test setup:: -- --nocapture`

Expected: PASS.

- [ ] **Step 6: Commit the shared setup state**

```bash
git add src/setup src/main.rs
git commit -m "feat(setup): add model configuration state machine" \
  -m "- share provider forms and persistence effects across setup entry points" \
  -m "- support dynamic custom model rows and safe cancellation"
```

### Task 6: Render and run automatic first-run setup

**Files:**

- Create: `src/ui/setup.rs`
- Modify: `src/ui/mod.rs`
- Modify: `src/main.rs`
- Modify: `src/cli.rs`
- Modify: `tests/cli.rs`
- Test: `src/ui/setup.rs`
- Test: `src/main.rs`
- Test: `tests/cli.rs`

**Interfaces:**

- Consumes: `SetupState`, `ProviderCatalog`, `ui::star::glint_star_rows()`, and terminal `KeyAction`
- Produces: `ui::setup::render(frame: &mut Frame, state: &SetupState, catalog: &ProviderCatalog)`
- Produces: `run_setup(terminal, manager, initial_state) -> anyhow::Result<SetupOutcome>`
- Consumes: `ConfigurationManager::available_providers()` to choose setup or chat before `App::new`

- [ ] **Step 1: Write failing renderer tests for the approved screens**

Render with `ratatui::backend::TestBackend` and flatten the buffer:

```rust
#[test]
fn welcome_renders_star_and_only_primary_actions() {
    let state = SetupState::welcome(&test_catalog());
    let rendered = render_setup(&state, 100, 30);

    assert!(rendered.contains("Add model"));
    assert!(rendered.contains("Exit"));
    assert!(!rendered.contains("Start Glint"));
}

#[test]
fn builtin_form_masks_key_and_lists_every_model() {
    let mut state = SetupState::builtin(test_catalog(), "deepseek");
    state.builtin_form_mut().unwrap().api_key.set("top-secret");
    let rendered = render_setup(&state, 100, 30);

    assert!(rendered.contains("deepseek-v4-flash"));
    assert!(rendered.contains("deepseek-v4-pro"));
    assert!(!rendered.contains("top-secret"));
}

#[test]
fn custom_rows_have_no_generated_labels_and_trailing_delete_icons() {
    let mut state = SetupState::custom_provider(test_catalog());
    state.custom_form_mut().unwrap().add_model_row();
    let rendered = render_setup(&state, 100, 30);

    assert!(!rendered.contains("Model ID"));
    assert_eq!(rendered.matches('×').count(), 2);
}
```

- [ ] **Step 2: Run renderer tests to verify RED**

Run: `cargo test ui::setup::tests -- --nocapture`

Expected: compilation fails because `ui::setup` is absent.

- [ ] **Step 3: Implement full-screen setup rendering**

Render the existing Glint star in the upper-left. Use the established theme constants and panel helpers. The welcome screen contains only `Add model` and `Exit`; provider management shows status/model count; built-in setup has one masked key field plus a read-only model list; custom setup has plain model boxes with `×` at the last column of each row and an `Add model` action beneath them.

Cursor placement must follow the focused `InputState`, including horizontal clipping for long provider URLs or model names. Errors render in the current form without replacing its content.

- [ ] **Step 4: Write failing startup and CLI contract tests**

Replace legacy CLI tests with:

```rust
#[test]
fn help_has_no_init_or_config_override() {
    let output = run_glint(["--help"], temp_home("help"));
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.contains("Usage: glint"));
    assert!(!stdout.contains("init"));
    assert!(!stdout.contains("--config"));
}

#[test]
fn legacy_init_is_rejected_by_clap() {
    let output = run_glint(["init"], temp_home("legacy-init"));

    assert!(!output.status.success());
    assert!(stderr(&output).contains("unexpected argument 'init'"));
}

#[test]
fn non_interactive_unconfigured_run_explains_interactive_setup() {
    let output = run_glint([], temp_home("non-interactive"));

    assert!(!output.status.success());
    assert!(stderr(&output).contains("run `glint` in an interactive terminal"));
}
```

The test helper sets `HOME` only on the spawned process, never on the test runner, and removes `GLINT_CONFIG` to prove it has no effect.

- [ ] **Step 5: Remove the init CLI and add bootstrap selection**

Reduce `Cli` to a clap `Parser` with version/about metadata and no fields or subcommands. In `main`:

```rust
let _cli = Cli::parse();
let workspace = std::env::current_dir().context("failed to resolve current directory")?;
let paths = GlintPaths::discover()?;
let mut manager = ConfigurationManager::discover(paths, &workspace)?;

if manager.available_providers()?.is_empty() && !io::stdout().is_terminal() {
    bail!("no model is configured; run `glint` in an interactive terminal to add one");
}
```

Enter raw mode once, then run setup before constructing `App`. `run_setup` draws, translates terminal key events through the existing `KeyInput`, applies setup effects, and returns `StartGlint` or `Exit`. On `StartGlint`, call `manager.build_runtime(&workspace)` and continue into the existing chat loop without leaving the alternate screen.

Guarantee terminal restoration on setup errors through the same post-loop cleanup path used by chat.

- [ ] **Step 6: Run setup, CLI, and terminal-loop tests**

Run:

```bash
cargo test ui::setup::tests main::tests -- --nocapture
cargo test --test cli -- --nocapture
```

Expected: PASS.

- [ ] **Step 7: Commit automatic first-run setup**

```bash
git add src/ui/setup.rs src/ui/mod.rs src/main.rs src/cli.rs tests/cli.rs
git commit -m "feat(setup): launch interactive model setup automatically" \
  -m "- render the approved first-run provider and custom-model forms" \
  -m "- remove manual init and legacy configuration CLI options"
```

### Task 7: Reuse setup from `/model` and persist switching

**Files:**

- Modify: `src/app.rs`
- Modify: `src/ui/mod.rs`
- Modify: `src/ui/model_picker.rs`
- Modify: `src/main.rs`
- Test: `src/app.rs`
- Test: `src/ui/model_picker.rs`
- Test: `src/ui/mod.rs`

**Interfaces:**

- Consumes: `ConfigurationManager::{available_providers,select_model}`
- Consumes: shared `SetupState`, `SetupEffect`, and `apply_setup_effect`
- Produces: `App::new(config: Config, configuration: ConfigurationManager) -> anyhow::Result<App>`
- Produces: `App.model_setup: Option<SetupState>` as the full-screen in-session management state
- Produces: a model picker whose provider list contains only available providers plus `Add model`

- [ ] **Step 1: Write failing configured-only picker and persistence tests**

```rust
#[test]
fn model_picker_lists_only_available_providers_and_add_model() {
    let mut app = app_with_configured_providers(["deepseek"]);

    app.open_model_picker();

    assert_eq!(
        app.model_picker_items(),
        vec!["DeepSeek".to_owned(), "Add model".to_owned()]
    );
}

#[test]
fn choosing_add_model_opens_shared_provider_management() {
    let mut app = app_with_configured_providers(["deepseek"]);
    app.open_model_picker();
    app.model_picker.as_mut().unwrap().selected_provider = 1;

    app.confirm_model_picker();

    assert!(app.model_picker.is_none());
    assert!(matches!(
        app.model_setup.as_ref().map(|setup| &setup.screen),
        Some(SetupScreen::Providers(_))
    ));
}

#[test]
fn selected_model_is_written_before_picker_closes() {
    let mut app = app_with_configured_providers(["deepseek"]);
    choose_model(&mut app, "deepseek", "deepseek-v4-pro");

    let persisted = app.configuration.user_config().llm.as_ref().unwrap();
    assert_eq!(persisted.provider, "deepseek");
    assert_eq!(persisted.model, "deepseek-v4-pro");
    assert_eq!(app.config.llm.model, "deepseek-v4-pro");
}

#[test]
fn deleting_last_provider_transitions_to_setup() {
    let mut app = app_with_configured_providers(["deepseek"]);
    delete_provider_through_setup(&mut app, "deepseek");

    assert!(app.configuration.available_providers().unwrap().is_empty());
    assert!(app.model_setup.is_some());
}
```

- [ ] **Step 2: Run picker tests to verify RED**

Run: `cargo test model_picker app::tests::selected_model app::tests::deleting_last -- --nocapture`

Expected: tests fail because `App` does not own the manager or shared setup state.

- [ ] **Step 3: Give `App` configuration ownership and route setup first**

Add `configuration: ConfigurationManager` and `model_setup: Option<SetupState>` to `App`. Update `App::new` and the test fixture. Route `model_setup` keyboard input before approvals, other pickers, slash menus, or chat input. Apply returned effects through the shared helper and refresh `config.llm` plus `config.model_catalog` only after persistence succeeds.

Add `#[cfg(test)] fn model_picker_items(&self) -> Vec<String>` as a projection over real picker rows; the other test helpers in this task (`choose_model` and `delete_provider_through_setup`) drive public update paths rather than mutating persisted state directly.

In `ui::render` and `render_prepared_document`, render `ui::setup` and return before any chat view whenever `app.model_setup` is present.

- [ ] **Step 4: Add `Add model` and durable model switching**

Keep the provider/model two-stage picker, but source its rows from `configuration.available_providers()`. Add a synthetic final provider row:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelPickerProviderRow {
    Provider { id: String, display_name: String },
    AddModel,
}
```

Selecting `AddModel` closes the picker and creates `SetupState::provider_list`. Selecting a real model calls `configuration.select_model`; on success replace `config.llm`, record the local `/model` exchange, and close the picker. On failure keep the picker open and display a redacted error.

After a setup mutation, recompute the available providers. Preserve the current selection if valid; otherwise adopt the manager's repaired selection. If no model remains, keep full-screen setup active and block prompt submission until a provider is saved and `Start Glint` is chosen.

- [ ] **Step 5: Render provider grouping and management entry**

Update `src/ui/model_picker.rs` so provider headings use `display_name`, model summaries come from `AvailableModel.metadata`, and the last row visibly reads `Add model`. Existing custom providers opened through setup reuse their stored URL and show a blank masked key field whose empty value preserves the credential.

Add TestBackend assertions that unconfigured catalog providers do not render, two custom providers with identical model names remain distinguishable, `Add model` renders last, and setup replaces the entire chat view.

- [ ] **Step 6: Run App and UI tests**

Run: `cargo test app:: ui:: -- --nocapture`

Expected: PASS.

- [ ] **Step 7: Commit unified `/model` management**

```bash
git add src/app.rs src/ui/mod.rs src/ui/model_picker.rs src/main.rs
git commit -m "feat(model): manage configured providers from model picker" \
  -m "- show only available models and persist model switching" \
  -m "- reuse first-run setup for adding, editing, and deleting providers"
```

### Task 8: Preserve extensions under the fixed configuration boundary

**Files:**

- Modify: `src/config.rs`
- Modify: `src/app.rs`
- Modify: `src/plugins/mod.rs`
- Modify: `src/runtime/mod.rs`
- Modify: `src/services/mcp/config.rs`
- Modify: `src/services/mcp/manager.rs`
- Modify: `src/transcript.rs`
- Test: `src/config.rs`
- Test: `src/app.rs`
- Test: `src/plugins/mod.rs`
- Test: `src/services/mcp/config.rs`
- Test: `src/services/mcp/manager.rs`

**Interfaces:**

- Consumes: optional raw `mcp`, `plugins`, and `lsp` values from `UserConfig`
- Consumes: `GlintPaths::root()`, `plugins()`, and `mcp()`
- Consumes: `GlintPaths::sessions()` for new transcript and archive storage
- Produces: the existing `Config::{mcp,plugins,lsp}` runtime fields and plugin-contributed MCP/LSP configuration
- Preserves: workspace roots for LSP analysis, shell/file tools, hooks, and default MCP process execution

- [ ] **Step 1: Write failing extension-boundary tests**

```rust
#[test]
fn extensions_load_from_user_config_without_llm_provider_metadata() {
    let yaml = r#"
version: 1
mcp:
  servers:
    local:
      transport: stdio
      command: demo-mcp
plugins:
  entries: []
lsp:
  servers: {}
"#;

    let user: UserConfig = serde_yaml::from_str(yaml).unwrap();
    let extensions = RuntimeExtensions::from_user_config(&user).unwrap();

    assert!(extensions.mcp.servers.contains_key("local"));
    assert!(extensions.plugins.entries.is_empty());
    assert!(extensions.lsp.servers.is_empty());
}

#[test]
fn relative_plugin_source_resolves_from_global_glint_root() {
    let root = Path::new("/users/alice/.glint");
    assert_eq!(
        resolve_user_path(Path::new("plugins/local"), root),
        Path::new("/users/alice/.glint/plugins/local")
    );
}

#[test]
fn default_mcp_process_cwd_remains_the_workspace() {
    let workspace = Path::new("/work/project");
    assert_eq!(resolve_server_cwd(None, workspace), workspace);
}
```

- [ ] **Step 2: Run extension tests to verify RED**

Run: `cargo test config::tests::extensions plugins::tests::relative_plugin services::mcp:: -- --nocapture`

Expected: at least the runtime extension loader and separated roots test fail.

- [ ] **Step 3: Parse extension sections and separate roots**

Create `RuntimeExtensions::from_user_config` in `src/config.rs` and deserialize each present raw YAML value with a path-specific error such as `failed to parse mcp in ~/.glint/config.yaml`. Omitted LSP keeps the current Rust default; an explicitly present `lsp.servers` replaces it, including an explicitly empty map.

Use this concrete boundary:

```rust
pub struct RuntimeExtensions {
    pub mcp: McpConfig,
    pub plugins: PluginsConfig,
    pub lsp: LspConfig,
}

impl RuntimeExtensions {
    pub fn from_user_config(config: &UserConfig) -> Result<Self>;
}
```

Pass two roots through runtime construction where needed:

- `workspace`: LSP root, tool root, hook working directory, transcript cwd label, and default MCP process cwd.
- `glint_root`: relative plugin-source base, plugin cache/state default, and MCP OAuth state root.

Change MCP OAuth path construction to accept the explicit `GlintPaths::mcp()` directory instead of re-reading `HOME`. Keep a configured relative MCP `cwd` workspace-relative because it controls the launched process, not where Glint stores configuration.

Change transcript construction to accept `GlintPaths::sessions()` explicitly. New conversations and archives are written below that directory. Preserve resume access to the old `~/.glint/projects` and `~/.glint/archive` locations as legacy search roots, so this layout change does not hide existing conversations. A resumed legacy conversation continues using its existing path; no automatic transcript migration is performed.

- [ ] **Step 4: Keep MCP and plugin mutations compatible with one YAML file**

Update `persist_mcp_server` tests to start from a minimal new-schema file and assert it adds only the `mcp` block without changing `version`, `llm`, `configured_providers`, or `custom_providers`. Continue using atomic replacement and the actual fixed `config_path` owned by `Config`.

Update plugin commands in `App` to pass `configuration.paths().root()` for plugin source/cache/state resolution while retaining the workspace for hooks and tools. Assert installation state remains below `~/.glint/plugins`.

Add transcript tests proving a new session is written below `~/.glint/sessions`, legacy summaries remain discoverable, and resuming a legacy session does not move or rewrite it until the user performs an existing transcript mutation.

- [ ] **Step 5: Run extension and runtime tests**

Run: `cargo test config:: plugins:: services::mcp:: runtime:: app::tests::mcp app::tests::plugin -- --nocapture`

Expected: PASS.

- [ ] **Step 6: Commit the extension boundary**

```bash
git add src/config.rs src/app.rs src/plugins/mod.rs src/runtime/mod.rs src/services/mcp/config.rs src/services/mcp/manager.rs src/transcript.rs
git commit -m "refactor(extensions): use the fixed Glint state root" \
  -m "- load MCP, plugin, and LSP settings from the concise user config" \
  -m "- separate global extension state from workspace execution roots"
```

### Task 9: Remove legacy artifacts, document behavior, and verify dist

**Files:**

- Remove: `config.yaml`
- Remove: `config.example.yaml`
- Modify: `README.md`
- Modify: `AGENTS.md`
- Modify: `EXTENSIONS.md`
- Modify: `.github/workflows/release.yml` only if regenerated dist output changes it
- Modify: `dist-workspace.toml` only if `dist plan` requires a generated adjustment
- Test: `tests/cli.rs`

**Interfaces:**

- Consumes: all preceding runtime and UI behavior
- Produces: install, first-run, upgrade, custom-provider, credential, and extension documentation matching the executable
- Preserves: cargo-dist 0.32, four existing release targets, shell installer, and `~/.local/bin` install path

- [ ] **Step 1: Add final legacy-removal assertions**

Extend `tests/cli.rs` so `--config`, `init`, and an arbitrary working-directory `config.yaml` are ignored/rejected as specified. Add a unit test around `GlintPaths` proving `GLINT_CONFIG` and `XDG_CONFIG_HOME` cannot alter `~/.glint/config.yaml`.

Run:

```bash
cargo test paths::tests -- --nocapture
cargo test --test cli -- --nocapture
```

Expected: PASS before deleting the obsolete source configurations.

- [ ] **Step 2: Remove the starter config and every code/document reference**

Delete the feature worktree copies of `config.yaml` and `config.example.yaml` after the embedded catalog tests prove all provider metadata was transferred. Do not delete or overwrite the modified `config.yaml` in the primary checkout. Search:

```bash
rg -n 'config\.example|glint init|--config|GLINT_CONFIG|XDG_CONFIG_HOME|api_key_env' \
  src tests README.md AGENTS.md EXTENSIONS.md Cargo.toml
```

Expected: no legacy setup/configuration references. References in historical design documents are allowed because the new spec explicitly supersedes them.

- [ ] **Step 3: Rewrite user and contributor documentation**

Document:

- install and immediate `glint` launch from a target workspace
- automatic welcome → Add model → provider → API key → provider list → Start Glint flow
- built-in providers enabling their complete shipped model lists
- `/model` switching and `Add model`
- custom provider name, base URL, masked API key, and dynamic model rows
- `~/.glint/config.yaml`, keyring preference, and protected `auth.json` fallback
- optional `mcp`, `plugins`, and `lsp` sections with their existing schemas
- the breaking upgrade behavior: no automatic import of working-directory config; extension blocks may be copied manually; API keys must be re-entered
- uninstall behavior that leaves `~/.glint` intact

Update the AGENTS runtime-config section so future changes do not reintroduce source-directory loading or environment-variable API keys.

- [ ] **Step 4: Run formatting, complete tests, and lint**

Run:

```bash
cargo fmt --check
cargo test
cargo clippy -- -D warnings
```

Expected: formatting clean, all tests pass, and clippy emits no warnings.

- [ ] **Step 5: Validate and build dist artifacts**

Run:

```bash
dist plan
dist build
```

Expected: dist accepts the manifest and builds the host archive plus installer metadata without requiring `config.example.yaml` or `assets/providers.yaml` as separate runtime files.

- [ ] **Step 6: Smoke-test outside the repository**

Extract the host archive into a fresh temporary directory, then run the extracted binary from a different empty working directory:

```bash
./glint --version
./glint --help
```

Expected: both commands succeed with no repository files. Then run the extracted binary with non-interactive stdio and an empty subprocess-scoped home.

Expected: it reports that no model is configured and instructs the user to run interactively; it does not report a missing provider catalog, prompt, or source `config.yaml`.

- [ ] **Step 7: Commit documentation and release cleanup**

```bash
git add -A config.yaml config.example.yaml README.md AGENTS.md EXTENSIONS.md tests/cli.rs \
  .github/workflows/release.yml dist-workspace.toml
git commit -m "docs(distribution): document automatic model setup" \
  -m "- replace init and environment-key instructions with the first-run flow" \
  -m "- verify the installed binary carries its provider catalog"
```

If dist regeneration does not change `.github/workflows/release.yml` or `dist-workspace.toml`, omit unchanged paths from `git add` rather than manufacturing a diff.

## Final Review Checklist

- [ ] Compare every requirement in the design spec against Tasks 1–9.
- [ ] Confirm the checked-in provider catalog contains every provider/model metadata entry currently shipped in `config.yaml`.
- [ ] Confirm no test or error snapshot contains a real-looking API key.
- [ ] Confirm setup drafts survive every persistence error path.
- [ ] Confirm selecting a built-in provider never asks for a model and never sends a network request.
- [ ] Confirm `/model` never displays an unconfigured provider except after entering `Add model`.
- [ ] Confirm deleting the active or final model produces the specified deterministic fallback.
- [ ] Confirm global extension state stays under `~/.glint` while coding tools and default MCP/LSP execution still use the workspace.
- [ ] Confirm the primary checkout's user-owned `config.yaml` was not modified.
- [ ] Request an independent code review after the full suite and dist smoke test pass.
