# Glint User Configuration and Model Setup Design

## Status

This design replaces the configuration discovery, `glint init`, and API-key behavior in the earlier standalone distribution design. The existing dist release targets, generated installer, embedded system prompt, and release workflow remain unchanged.

## Goals

- Make an installed `glint` binary runnable from any working directory without repository files.
- Keep all persistent Glint state below `~/.glint` while leaving the current working directory as the workspace on which tools operate.
- Store only user-controlled settings in the user configuration.
- Compile stable built-in provider metadata into the binary.
- Guide an unconfigured user through model setup inside the TUI instead of requiring `glint init`.
- Let users configure multiple built-in and custom providers and expose only configured models in `/model`.
- Keep API keys out of YAML, logs, transcripts, and command-line arguments.

## Non-goals

- Automatic migration from a legacy working-directory `config.yaml`.
- Automatic provider connectivity tests during setup.
- Fetching model lists from provider APIs.
- A self-updater, non-interactive chat mode, or native Windows release.
- Editing or renaming an existing custom provider's display name. A rename is modeled as deleting and recreating the provider.

## Persistent Layout

Glint uses one fixed state root:

```text
~/.glint/
├── config.yaml
├── auth.json        # only when the system keyring cannot be used
├── plugins/         # managed plugin files and cache, when present
├── mcp/             # MCP-owned persistent state, when present
└── sessions/        # conversation persistence
```

`~/.glint/config.yaml` is the only user-editable settings file. Empty optional sections are omitted rather than materialized with defaults. Relative paths in settings are resolved from `~/.glint`; the process working directory remains the workspace for file and shell tools.

Glint no longer supports:

- `--config PATH`
- `GLINT_CONFIG`
- project-local `.glint/config.yaml`
- a current-working-directory `config.yaml` fallback
- the `glint init` subcommand

`glint --help` and `glint --version` remain usable without configuration. When standard input or output is not an interactive terminal and no model is configured, `glint` prints an actionable error telling the user to run it in an interactive terminal and exits without attempting to render the TUI.

## Built-in Provider Catalog

Stable provider metadata moves to `assets/providers.yaml` and is compiled into the executable with `include_str!`. It contains:

- canonical provider ID and display name
- description and OpenAI-compatible base URL
- default model
- supported model names and picker metadata
- pricing unit and token prices
- context and output limits
- optional prompt-cache settings

This catalog is not copied to `~/.glint/config.yaml`. A malformed embedded catalog is a build or test failure, not a runtime state the user can repair.

Selecting a built-in provider during setup requires only its API key. After saving, every model in that provider's embedded catalog becomes available. Glint does not call `/models` or send a chat request as part of saving because provider APIs vary and a chat request can incur cost.

## User Configuration Schema

A representative configured file is:

```yaml
version: 1

llm:
  provider: deepseek
  model: deepseek-chat
  temperature: 0.7
  max_tokens: 8196

configured_providers:
  - deepseek
  - openai

custom_providers:
  Team Gateway:
    base_url: https://llm.example.com/v1
    models:
      - code-large
      - code-fast
```

Only populated optional sections are written. In particular, `configured_providers`, `custom_providers`, `mcp`, `plugins`, and `lsp` are omitted when empty. The `version` field supports future schema migrations.

`configured_providers` records only which built-in catalog entries the user enabled. It contains no endpoint, price, or model metadata. This explicit index is necessary because operating-system keyrings do not provide a portable way to enumerate Glint credentials.

Each key under `custom_providers` is the user-defined provider display name and identity. Names are trimmed, case-insensitively unique, and may not collide with a built-in provider ID or display name. A custom provider owns only its base URL and non-empty, de-duplicated model-name list. It does not own copied pricing or other built-in metadata.

The `llm` selection is present whenever at least one configured model exists. When no model exists, `llm` may be absent so extension settings can remain on disk while the setup UI is shown. The first successful provider save creates `llm` with the provider's first/default model and the standard temperature and maximum-token defaults. Later model switches update only `llm.provider` and `llm.model`.

## Credential Storage

Credential identities are stable strings:

- `builtin:<provider-id>` for a built-in provider
- `custom:<provider-name>` for a custom provider

The credential service prefers the operating-system keyring. On a fresh installation, if the keyring is unavailable, it creates `~/.glint/auth.json` and uses that file consistently on later runs. The directory is created with mode `0700` and the file with mode `0600` on Unix. The file contains only credential identities and API keys; it is never merged into the user configuration.

If `auth.json` exists, the file backend remains authoritative. Automatic migration between file and keyring backends is outside this change. If configured providers exist, no file backend exists, and the previously used keyring is temporarily unavailable, Glint reports the credential-store failure and offers credential repair rather than silently creating a second store.

The UI masks API-key input. When editing a configured provider, an empty key field retains the current secret and a non-empty value replaces it. Secret values are never included in debug formatting, errors, transcripts, or logs.

## Configured Model Rules

Glint computes the available model set from the catalog, user configuration, and readable credentials:

- A built-in provider contributes all catalog models when its ID appears in `configured_providers` and its credential is readable and non-empty.
- A custom provider contributes its configured model names when it has a valid base URL, at least one model, and a readable non-empty credential.
- A configured provider whose credential is missing or unavailable contributes no models and is shown as needing credential repair.

Zero available models means setup is required. One or more available models means the chat application can start. If the persisted current selection is absent from the available set, Glint selects the embedded default for the first available built-in provider, or the first model of the first available custom provider, and atomically persists that repaired selection.

Provider and model ordering is deterministic. Built-in providers follow catalog order; custom providers are sorted case-insensitively by display name. Models follow their catalog or configuration order.

## First-run Experience

Configuration loading and available-model resolution happen before the chat `App` is constructed. With no available models, Glint enters a setup state machine inside the normal terminal lifecycle.

The welcome screen shows the Glint star mark in the upper-left and only two actions:

- `Add model`
- `Exit`

`Add model` opens a provider list containing every built-in provider followed by existing custom providers and `Custom provider`. Choosing a built-in provider opens a form with a masked API-key field and a read-only list of all supported catalog models. Saving a non-empty key enables the provider and returns to the provider list. The provider row then displays its configured status and model count.

Once at least one model is available, `Start Glint` is enabled in the provider list. This lets a user configure several providers before entering the chat interface. If no current model exists, the first saved provider's embedded default model, or first custom model, becomes current.

Saving performs local validation only. A provider authentication or connectivity problem is reported on the first real model request and can then be repaired through `/model`.

## Custom Provider Form

A new custom provider form contains:

- provider name
- OpenAI-compatible base URL
- masked API key
- model-name rows

The model section initially contains one plain input box. It does not display labels such as `Model ID 1`. A keyboard-focusable delete icon appears at the far right of the same row. `Add model` appends another identical row below it. The form always retains at least one row; deleting the last row clears it. Saving requires at least one non-empty model name, trims whitespace, preserves entry order, and reports exact duplicates for correction.

Selecting an existing custom provider from `Add model` reuses its name, URL, and credential. The user can append or remove model rows, update the URL, or replace the API key. Removing every model is not a substitute for deleting a provider and is rejected; provider deletion is a separate confirmed action.

Canceling any setup form returns without writing credentials or configuration. A failed save leaves every entered value in place and renders the error in the current form.

## `/model` Behavior

The normal `/model` picker lists only providers that currently contribute available models. Models are grouped under the provider display name, so separately named custom providers remain distinguishable even when they contain identical model names.

Selecting a model immediately updates the running provider and atomically persists `llm.provider` and `llm.model`. The picker contains an `Add model` entry that opens the same provider-management state and forms used by first-run setup.

From provider management:

- a configured built-in provider can have its API key replaced
- an existing custom provider can have models appended or removed while reusing its URL and API key
- a provider can be deleted after confirmation

Deleting a provider removes its configuration and credential. If the active model is removed, Glint switches to the first remaining available model and persists it. If no available model remains, the chat application transitions to setup instead of continuing with an invalid runtime provider.

## Save Consistency and Failure Handling

User YAML is written to a sibling temporary file and atomically renamed over `config.yaml`. The original file is not truncated in place. A malformed existing YAML file is never treated as an empty configuration and never overwritten; Glint reports its path and parse error.

A provider mutation is staged and validated before persistent state changes. Credential updates keep the previous value long enough to perform a compensating rollback if the configuration write fails. The in-memory catalog and active model are updated only after both persistence operations succeed. If rollback itself fails, Glint reports both errors without exposing either secret.

For deletion, failure to remove the credential prevents the deletion from being reported as successful. A stale credential that cannot be removed is never considered enough to configure a provider because the configuration index remains authoritative.

Validation errors are attached to their form fields where possible. Filesystem, keyring, or serialization errors appear as a form-level message and preserve the draft. Runtime provider errors do not mutate saved configuration automatically.

## Legacy Configuration

The previous working-directory `config.yaml` schema mixed user selection, provider catalog data, extension settings, and environment-variable credential references. The new release does not search for or automatically import it. Automatic import would make the configuration depend on the directory from which an installed binary happened to start, which conflicts with the fixed state root.

After upgrading, a user with only the old format sees the first-run setup UI and re-enters API keys. Existing `mcp`, `plugins`, and `lsp` blocks can be manually copied into `~/.glint/config.yaml`. Documentation will call out this breaking change. No API key is imported from an environment variable or legacy file.

## Component Boundaries

- `assets/providers.yaml` owns built-in provider data.
- `src/provider_catalog.rs` parses the embedded catalog, merges custom providers, resolves ordering, and computes available models. It does not perform I/O for secrets.
- `src/config.rs` owns the user schema, fixed `~/.glint` paths, validation, defaults, and atomic YAML persistence.
- `src/credentials.rs` exposes a small credential-store interface with keyring and protected-file implementations.
- `src/setup/` owns setup state, navigation, draft forms, validation, and save intents. It depends on catalog/config/credential interfaces rather than concrete storage.
- `src/ui/setup.rs` renders setup state and maps no side effects of its own.
- The existing model picker consumes the unified runtime catalog and delegates `Add model` to the setup state.
- `src/main.rs` loads state, chooses setup or chat startup, and starts MCP, plugins, and LSP only after a valid runtime model exists.

The existing `App::update` event boundary remains authoritative for TUI state changes. Rendering stays side-effect free. Credential and configuration writes are represented as explicit setup actions whose results return through the state machine.

## Packaging and Documentation

Because the provider catalog and system prompt are compiled into the binary, the dist archive needs no runtime YAML or prompt asset. Existing macOS and Linux/WSL release targets and the shell installer remain unchanged.

The README and CLI help will describe automatic first-run setup, the fixed `~/.glint` location, credential storage, custom providers, `/model`, upgrade behavior, and manual transfer of legacy extension blocks. References to `glint init`, `--config`, `GLINT_CONFIG`, and environment-variable API-key setup will be removed.

## Verification

Implementation follows test-driven development. Coverage includes:

- embedded catalog parsing, required defaults, model metadata, and invalid-catalog rejection
- user YAML defaults, version validation, omission of empty optional sections, and atomic replacement behavior
- built-in/custom name collisions, URL validation, model trimming, stable ordering, and duplicate-model rejection
- a replaceable in-memory credential store plus keyring/file backend behavior and protected Unix permissions
- available-model computation, missing-credential repair, first-provider selection, invalid-current-model repair, and last-model removal
- setup reducers for welcome, provider list, built-in save, custom save, cancellation, errors, and deletion confirmation
- custom model-row creation, same-row deletion behavior, retention of one row, and absence of generated row labels
- `/model` filtering, grouping, immediate persistence, and the `Add model` transition
- Ratatui `TestBackend` snapshots or assertions for the key first-run and model-management screens
- CLI integration behavior for `--help`, `--version`, non-interactive unconfigured startup, and removal of legacy options and `init`
- dist smoke tests from outside the source tree proving that help/version and the embedded provider catalog work without repository assets

Before completion, run:

```bash
cargo fmt --check
cargo test
cargo clippy -- -D warnings
dist plan
dist build
```

The dist host artifact is then extracted or installed into a temporary location and exercised outside the repository.

## Acceptance Criteria

- A fresh interactive `glint` launch opens the approved setup UI without any manual initialization command.
- Configuring only a built-in API key makes all of that provider's catalog models available.
- Multiple built-in and custom providers can coexist, and `/model` displays only their configured models under distinct provider names.
- Custom model rows match the approved interaction: one initial unlabeled box, addable rows, and a trailing same-row delete icon.
- User settings live in one concise `~/.glint/config.yaml`; built-in metadata does not.
- API keys live in the system keyring or protected fallback file and are never exposed through configuration, logs, or transcripts.
- Glint starts from arbitrary working directories and the dist archive carries everything required at runtime.
- Invalid or partial persistence cannot silently overwrite a valid configuration or make an unavailable provider active.
