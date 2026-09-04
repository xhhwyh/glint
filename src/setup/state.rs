use std::fmt;

use anyhow::Result;

use crate::{
    configuration::{ConfigurationManager, ConfigurationMutationErrorKind, ProviderStatus},
    credentials::CredentialStoreStatus,
    event::KeyAction,
    input::InputState,
    provider_catalog::ProviderCatalog,
};

#[derive(Clone)]
pub struct SetupState {
    pub screen: SetupScreen,
    pub error: Option<String>,
    pub notice: Option<String>,
    catalog: ProviderCatalog,
}

const DEGRADED_CREDENTIAL_REPAIR_NOTICE: &str = "The system credential store is unavailable.\nRe-enter an API key to repair this provider.\nGlint will switch to its protected auth.json file.";

impl SetupState {
    pub fn welcome(catalog: &ProviderCatalog) -> Self {
        Self {
            screen: SetupScreen::Welcome(WelcomeState::default()),
            error: None,
            notice: None,
            catalog: catalog.clone(),
        }
    }

    pub fn provider_list(
        catalog: &ProviderCatalog,
        manager: &ConfigurationManager,
    ) -> Result<Self> {
        let mut state = Self::welcome(catalog);
        state.refresh_provider_list(manager)?;
        Ok(state)
    }

    pub fn custom_provider(catalog: ProviderCatalog) -> Self {
        let list = ProviderListState::from_catalog(&catalog);
        Self {
            screen: SetupScreen::Custom(CustomProviderForm::new_with_list(list)),
            error: None,
            notice: None,
            catalog,
        }
    }

    /// Opens a catalog-defined built-in form. Unknown IDs return to the provider list with a
    /// display-safe error instead of constructing an invalid form.
    pub fn builtin(catalog: ProviderCatalog, provider_id: &str) -> Self {
        let list = ProviderListState::from_catalog(&catalog);
        let Some(provider) = catalog.builtin(provider_id) else {
            return Self {
                screen: SetupScreen::Providers(list),
                error: Some(format!("Built-in provider '{provider_id}' is not defined.")),
                notice: None,
                catalog,
            };
        };
        Self {
            screen: SetupScreen::Builtin(BuiltinProviderForm::new(
                provider.id.clone(),
                provider.name.clone(),
                provider
                    .models
                    .iter()
                    .map(|model| model.name.clone())
                    .collect(),
                list,
            )),
            error: None,
            notice: None,
            catalog,
        }
    }

    pub fn builtin_form_mut(&mut self) -> Option<&mut BuiltinProviderForm> {
        match &mut self.screen {
            SetupScreen::Builtin(form) => Some(form),
            _ => None,
        }
    }

    pub fn custom_form_mut(&mut self) -> Option<&mut CustomProviderForm> {
        match &mut self.screen {
            SetupScreen::Custom(form) => Some(form),
            _ => None,
        }
    }

    pub fn update(&mut self, action: KeyAction) -> Option<SetupEffect> {
        let mut next_screen = None;
        let effect = match &mut self.screen {
            SetupScreen::Welcome(welcome) => {
                welcome.navigate(action);
                if action == KeyAction::Submit {
                    match welcome.focus {
                        WelcomeFocus::AddModel => {
                            next_screen = Some(SetupScreen::Providers(
                                ProviderListState::from_catalog(&self.catalog),
                            ));
                            None
                        }
                        WelcomeFocus::Exit => Some(SetupEffect::Exit),
                    }
                } else {
                    None
                }
            }
            SetupScreen::Providers(list) => {
                list.navigate(action);
                if action == KeyAction::Delete {
                    if let Some(row) = list.selected().cloned().filter(ProviderListRow::can_delete)
                    {
                        next_screen = Some(SetupScreen::ConfirmDelete(DeleteProviderState {
                            provider_id: row
                                .provider_id()
                                .expect("deletable rows have an ID")
                                .to_owned(),
                            display_name: row.display_name().to_owned(),
                            focus: DeleteFocus::Confirm,
                            list: Box::new(list.clone()),
                        }));
                    }
                    None
                } else if action == KeyAction::Submit {
                    match list.selected().cloned() {
                        Some(ProviderListRow::Builtin { provider_id, .. }) => {
                            let provider = self
                                .catalog
                                .builtin(&provider_id)
                                .expect("provider list only contains catalog providers");
                            next_screen = Some(SetupScreen::Builtin(BuiltinProviderForm::new(
                                provider_id,
                                provider.name.clone(),
                                provider
                                    .models
                                    .iter()
                                    .map(|model| model.name.clone())
                                    .collect(),
                                list.clone(),
                            )));
                            None
                        }
                        Some(ProviderListRow::Custom {
                            name,
                            base_url,
                            models,
                            ..
                        }) => {
                            next_screen = Some(SetupScreen::Custom(CustomProviderForm::existing(
                                name,
                                base_url,
                                models,
                                list.clone(),
                            )));
                            None
                        }
                        Some(ProviderListRow::AddCustom) => {
                            next_screen = Some(SetupScreen::Custom(
                                CustomProviderForm::new_with_list(list.clone()),
                            ));
                            None
                        }
                        Some(ProviderListRow::StartGlint) => Some(SetupEffect::StartGlint),
                        None => None,
                    }
                } else {
                    None
                }
            }
            SetupScreen::Builtin(form) => match form.handle(action) {
                FormAction::None => None,
                FormAction::Cancel => {
                    next_screen = Some(SetupScreen::Providers((*form.list).clone()));
                    None
                }
                FormAction::Save => Some(SetupEffect::SaveBuiltin {
                    provider_id: form.provider_id.clone(),
                    api_key: nonblank_input(&form.api_key),
                }),
            },
            SetupScreen::Custom(form) => match form.handle(action) {
                FormAction::None => None,
                FormAction::Cancel => {
                    next_screen = Some(SetupScreen::Providers((*form.list).clone()));
                    None
                }
                FormAction::Save => Some(SetupEffect::SaveCustom {
                    name: form
                        .existing_identity
                        .clone()
                        .unwrap_or_else(|| form.name.value.clone()),
                    base_url: form.base_url.value.clone(),
                    api_key: nonblank_input(&form.api_key),
                    models: form
                        .models
                        .iter()
                        .map(|model| model.value.clone())
                        .collect(),
                }),
            },
            SetupScreen::ConfirmDelete(confirm) => {
                confirm.navigate(action);
                match action {
                    KeyAction::Escape => {
                        next_screen = Some(SetupScreen::Providers((*confirm.list).clone()));
                        None
                    }
                    KeyAction::Submit => match confirm.focus {
                        DeleteFocus::Cancel => {
                            next_screen = Some(SetupScreen::Providers((*confirm.list).clone()));
                            None
                        }
                        DeleteFocus::Confirm => Some(SetupEffect::DeleteProvider {
                            provider_id: confirm.provider_id.clone(),
                        }),
                    },
                    _ => None,
                }
            }
        };
        if let Some(next_screen) = next_screen {
            self.screen = next_screen;
        }
        effect
    }

    fn refresh_provider_list(&mut self, manager: &ConfigurationManager) -> Result<()> {
        let statuses = manager.provider_statuses()?;
        let available_models = manager
            .available_providers()?
            .iter()
            .map(|provider| provider.models.len())
            .sum::<usize>();
        self.screen = SetupScreen::Providers(ProviderListState::from_manager(
            &self.catalog,
            manager,
            &statuses,
            available_models > 0,
        ));
        self.notice = match manager.credential_store_status() {
            CredentialStoreStatus::Ready => None,
            CredentialStoreStatus::KeyringUnavailable { .. } => {
                Some(DEGRADED_CREDENTIAL_REPAIR_NOTICE.to_owned())
            }
        };
        Ok(())
    }
}

impl fmt::Debug for SetupState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SetupState")
            .field("screen", &self.screen)
            .field("error", &self.error)
            .field("notice", &self.notice)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub enum SetupScreen {
    Welcome(WelcomeState),
    Providers(ProviderListState),
    Builtin(BuiltinProviderForm),
    Custom(CustomProviderForm),
    ConfirmDelete(DeleteProviderState),
}

impl fmt::Debug for SetupScreen {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Welcome(state) => formatter.debug_tuple("Welcome").field(state).finish(),
            Self::Providers(state) => formatter.debug_tuple("Providers").field(state).finish(),
            Self::Builtin(form) => formatter.debug_tuple("Builtin").field(form).finish(),
            Self::Custom(form) => formatter.debug_tuple("Custom").field(form).finish(),
            Self::ConfirmDelete(state) => {
                formatter.debug_tuple("ConfirmDelete").field(state).finish()
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct WelcomeState {
    pub focus: WelcomeFocus,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WelcomeFocus {
    #[default]
    AddModel,
    Exit,
}

impl WelcomeState {
    fn navigate(&mut self, action: KeyAction) {
        if matches!(action, KeyAction::Tab | KeyAction::Up | KeyAction::Down) {
            self.focus = match self.focus {
                WelcomeFocus::AddModel => WelcomeFocus::Exit,
                WelcomeFocus::Exit => WelcomeFocus::AddModel,
            };
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProviderListState {
    pub rows: Vec<ProviderListRow>,
    pub focus: usize,
}

impl ProviderListState {
    fn from_catalog(catalog: &ProviderCatalog) -> Self {
        let mut rows = catalog
            .providers()
            .iter()
            .map(|provider| ProviderListRow::Builtin {
                provider_id: provider.id.clone(),
                display_name: provider.name.clone(),
                configured: false,
                needs_credential: false,
                model_count: provider.models.len(),
            })
            .collect::<Vec<_>>();
        rows.push(ProviderListRow::AddCustom);
        Self { rows, focus: 0 }
    }

    fn from_manager(
        catalog: &ProviderCatalog,
        manager: &ConfigurationManager,
        statuses: &[ProviderStatus],
        has_available_models: bool,
    ) -> Self {
        let mut rows = Vec::new();
        for provider in catalog.providers() {
            let status = statuses
                .iter()
                .find(|status| status.builtin && status.id == provider.id)
                .expect("all catalog providers have a status");
            rows.push(ProviderListRow::Builtin {
                provider_id: status.id.clone(),
                display_name: status.display_name.clone(),
                configured: status.configured,
                needs_credential: status.needs_credential,
                model_count: status.model_count,
            });
        }
        for status in statuses.iter().filter(|status| !status.builtin) {
            let provider = manager
                .user_config()
                .custom_providers
                .get(&status.id)
                .expect("custom provider status has persisted details");
            rows.push(ProviderListRow::Custom {
                name: status.display_name.clone(),
                base_url: provider.base_url.clone(),
                models: provider.models.clone(),
                needs_credential: status.needs_credential,
            });
        }
        rows.push(ProviderListRow::AddCustom);
        if has_available_models {
            rows.push(ProviderListRow::StartGlint);
        }
        Self { rows, focus: 0 }
    }

    pub fn selected(&self) -> Option<&ProviderListRow> {
        self.rows.get(self.focus)
    }

    fn navigate(&mut self, action: KeyAction) {
        if self.rows.is_empty() {
            return;
        }
        match action {
            KeyAction::Down | KeyAction::Tab => self.focus = (self.focus + 1) % self.rows.len(),
            KeyAction::Up => self.focus = (self.focus + self.rows.len() - 1) % self.rows.len(),
            _ => {}
        }
    }
}

#[derive(Clone, Debug)]
pub enum ProviderListRow {
    Builtin {
        provider_id: String,
        display_name: String,
        configured: bool,
        needs_credential: bool,
        model_count: usize,
    },
    Custom {
        name: String,
        base_url: String,
        models: Vec<String>,
        needs_credential: bool,
    },
    AddCustom,
    StartGlint,
}

impl ProviderListRow {
    pub fn provider_id(&self) -> Option<&str> {
        match self {
            Self::Builtin { provider_id, .. } => Some(provider_id),
            Self::Custom { name, .. } => Some(name),
            Self::AddCustom | Self::StartGlint => None,
        }
    }

    pub fn display_name(&self) -> &str {
        match self {
            Self::Builtin { display_name, .. } => display_name,
            Self::Custom { name, .. } => name,
            Self::AddCustom => "Custom provider",
            Self::StartGlint => "Start Glint",
        }
    }

    pub fn configured(&self) -> bool {
        matches!(
            self,
            Self::Builtin {
                configured: true,
                ..
            } | Self::Custom { .. }
        )
    }

    pub fn needs_credential(&self) -> bool {
        match self {
            Self::Builtin {
                needs_credential, ..
            }
            | Self::Custom {
                needs_credential, ..
            } => *needs_credential,
            Self::AddCustom | Self::StartGlint => false,
        }
    }

    pub fn model_count(&self) -> usize {
        match self {
            Self::Builtin { model_count, .. } => *model_count,
            Self::Custom { models, .. } => models.len(),
            Self::AddCustom | Self::StartGlint => 0,
        }
    }

    pub fn can_delete(&self) -> bool {
        matches!(
            self,
            Self::Builtin {
                configured: true,
                ..
            } | Self::Custom { .. }
        )
    }
}

#[derive(Clone)]
pub struct BuiltinProviderForm {
    pub provider_id: String,
    pub display_name: String,
    pub models: Vec<String>,
    pub api_key: InputState,
    pub focus: BuiltinFocus,
    list: Box<ProviderListState>,
}

impl BuiltinProviderForm {
    fn new(
        provider_id: String,
        display_name: String,
        models: Vec<String>,
        list: ProviderListState,
    ) -> Self {
        Self {
            provider_id,
            display_name,
            models,
            api_key: InputState::default(),
            focus: BuiltinFocus::ApiKey,
            list: Box::new(list),
        }
    }

    fn handle(&mut self, action: KeyAction) -> FormAction {
        match action {
            KeyAction::Escape => FormAction::Cancel,
            KeyAction::Tab | KeyAction::Down => {
                self.focus = self.focus.next();
                FormAction::None
            }
            KeyAction::Up => {
                self.focus = self.focus.previous();
                FormAction::None
            }
            KeyAction::Submit if self.focus == BuiltinFocus::Save => FormAction::Save,
            KeyAction::Submit if self.focus == BuiltinFocus::Cancel => FormAction::Cancel,
            KeyAction::Submit => {
                self.focus = self.focus.next();
                FormAction::None
            }
            _ if self.focus == BuiltinFocus::ApiKey => {
                edit_input(&mut self.api_key, action);
                FormAction::None
            }
            _ => FormAction::None,
        }
    }
}

impl fmt::Debug for BuiltinProviderForm {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BuiltinProviderForm")
            .field("provider_id", &self.provider_id)
            .field("display_name", &self.display_name)
            .field("models", &self.models)
            .field("api_key", &"[REDACTED]")
            .field("focus", &self.focus)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinFocus {
    ApiKey,
    Save,
    Cancel,
}

impl BuiltinFocus {
    fn next(self) -> Self {
        match self {
            Self::ApiKey => Self::Save,
            Self::Save => Self::Cancel,
            Self::Cancel => Self::ApiKey,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::ApiKey => Self::Cancel,
            Self::Save => Self::ApiKey,
            Self::Cancel => Self::Save,
        }
    }
}

#[derive(Clone)]
pub struct CustomProviderForm {
    pub name: InputState,
    pub base_url: InputState,
    pub api_key: InputState,
    pub models: Vec<InputState>,
    pub focus: CustomFocus,
    existing_identity: Option<String>,
    list: Box<ProviderListState>,
}

impl CustomProviderForm {
    pub fn new() -> Self {
        Self::new_with_list(ProviderListState {
            rows: Vec::new(),
            focus: 0,
        })
    }

    fn new_with_list(list: ProviderListState) -> Self {
        Self {
            name: InputState::default(),
            base_url: InputState::default(),
            api_key: InputState::default(),
            models: vec![InputState::default()],
            focus: CustomFocus::Name,
            existing_identity: None,
            list: Box::new(list),
        }
    }

    fn existing(
        name: String,
        base_url: String,
        models: Vec<String>,
        list: ProviderListState,
    ) -> Self {
        let mut form = Self::new_with_list(list);
        form.name.set(name.clone());
        form.base_url.set(base_url);
        form.models = models
            .into_iter()
            .map(|model| {
                let mut input = InputState::default();
                input.set(model);
                input
            })
            .collect();
        if form.models.is_empty() {
            form.models.push(InputState::default());
        }
        form.existing_identity = Some(name);
        form.focus = CustomFocus::BaseUrl;
        form
    }

    pub fn name_is_read_only(&self) -> bool {
        self.existing_identity.is_some()
    }

    pub fn add_model_row(&mut self) {
        self.models.push(InputState::default());
    }

    pub fn delete_model_row(&mut self, index: usize) {
        if index >= self.models.len() {
            return;
        }
        if self.models.len() == 1 {
            self.models[0] = InputState::default();
            self.focus = CustomFocus::Model(0);
            return;
        }
        self.models.remove(index);
        self.focus = match self.focus {
            CustomFocus::Model(current) | CustomFocus::DeleteModel(current)
                if current >= self.models.len() =>
            {
                CustomFocus::Model(self.models.len() - 1)
            }
            CustomFocus::Model(current) if current > index => CustomFocus::Model(current - 1),
            CustomFocus::DeleteModel(current) if current > index => {
                CustomFocus::DeleteModel(current - 1)
            }
            CustomFocus::DeleteModel(current) if current == index => {
                CustomFocus::Model(index.min(self.models.len() - 1))
            }
            focus => focus,
        };
    }

    fn handle(&mut self, action: KeyAction) -> FormAction {
        match action {
            KeyAction::Escape => FormAction::Cancel,
            KeyAction::Tab | KeyAction::Down => {
                self.focus = self
                    .focus
                    .next(self.models.len(), !self.name_is_read_only());
                FormAction::None
            }
            KeyAction::Up => {
                self.focus = self
                    .focus
                    .previous(self.models.len(), !self.name_is_read_only());
                FormAction::None
            }
            KeyAction::Submit if self.focus == CustomFocus::AddModel => {
                self.add_model_row();
                self.focus = CustomFocus::Model(self.models.len() - 1);
                FormAction::None
            }
            KeyAction::Submit | KeyAction::Delete
                if matches!(self.focus, CustomFocus::DeleteModel(_)) =>
            {
                let CustomFocus::DeleteModel(index) = self.focus else {
                    unreachable!();
                };
                self.delete_model_row(index);
                FormAction::None
            }
            KeyAction::Submit if self.focus == CustomFocus::Save => FormAction::Save,
            KeyAction::Submit if self.focus == CustomFocus::Cancel => FormAction::Cancel,
            KeyAction::Submit => {
                self.focus = self
                    .focus
                    .next(self.models.len(), !self.name_is_read_only());
                FormAction::None
            }
            _ => {
                match self.focus {
                    CustomFocus::Name => edit_input(&mut self.name, action),
                    CustomFocus::BaseUrl => edit_input(&mut self.base_url, action),
                    CustomFocus::ApiKey => edit_input(&mut self.api_key, action),
                    CustomFocus::Model(index) => {
                        if let Some(model) = self.models.get_mut(index) {
                            edit_input(model, action);
                        }
                    }
                    CustomFocus::DeleteModel(_)
                    | CustomFocus::AddModel
                    | CustomFocus::Save
                    | CustomFocus::Cancel => {}
                }
                FormAction::None
            }
        }
    }
}

impl fmt::Debug for CustomProviderForm {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CustomProviderForm")
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("api_key", &"[REDACTED]")
            .field("models", &self.models)
            .field("focus", &self.focus)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CustomFocus {
    Name,
    BaseUrl,
    ApiKey,
    Model(usize),
    DeleteModel(usize),
    AddModel,
    Save,
    Cancel,
}

impl CustomFocus {
    fn next(self, models: usize, name_editable: bool) -> Self {
        match self {
            Self::Name => Self::BaseUrl,
            Self::BaseUrl => Self::ApiKey,
            Self::ApiKey => Self::Model(0),
            Self::Model(index) => Self::DeleteModel(index),
            Self::DeleteModel(index) if index + 1 < models => Self::Model(index + 1),
            Self::DeleteModel(_) => Self::AddModel,
            Self::AddModel => Self::Save,
            Self::Save => Self::Cancel,
            Self::Cancel if name_editable => Self::Name,
            Self::Cancel => Self::BaseUrl,
        }
    }

    fn previous(self, models: usize, name_editable: bool) -> Self {
        match self {
            Self::Name => Self::Cancel,
            Self::BaseUrl if name_editable => Self::Name,
            Self::BaseUrl => Self::Cancel,
            Self::ApiKey => Self::BaseUrl,
            Self::Model(0) => Self::ApiKey,
            Self::Model(index) => Self::DeleteModel(index - 1),
            Self::DeleteModel(index) => Self::Model(index),
            Self::AddModel => Self::DeleteModel(models.saturating_sub(1)),
            Self::Save => Self::AddModel,
            Self::Cancel => Self::Save,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DeleteProviderState {
    pub provider_id: String,
    pub display_name: String,
    pub focus: DeleteFocus,
    list: Box<ProviderListState>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeleteFocus {
    #[default]
    Confirm,
    Cancel,
}

impl DeleteProviderState {
    fn navigate(&mut self, action: KeyAction) {
        if matches!(
            action,
            KeyAction::Tab | KeyAction::Left | KeyAction::Right | KeyAction::Up | KeyAction::Down
        ) {
            self.focus = match self.focus {
                DeleteFocus::Confirm => DeleteFocus::Cancel,
                DeleteFocus::Cancel => DeleteFocus::Confirm,
            };
        }
    }
}

#[derive(Clone, Copy)]
enum FormAction {
    None,
    Save,
    Cancel,
}

#[derive(Clone, PartialEq, Eq)]
pub enum SetupEffect {
    SaveBuiltin {
        provider_id: String,
        api_key: Option<String>,
    },
    SaveCustom {
        name: String,
        base_url: String,
        api_key: Option<String>,
        models: Vec<String>,
    },
    DeleteProvider {
        provider_id: String,
    },
    StartGlint,
    Exit,
}

impl fmt::Debug for SetupEffect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SaveBuiltin { provider_id, .. } => formatter
                .debug_struct("SaveBuiltin")
                .field("provider_id", provider_id)
                .field("api_key", &"[REDACTED]")
                .finish(),
            Self::SaveCustom {
                name,
                base_url,
                models,
                ..
            } => formatter
                .debug_struct("SaveCustom")
                .field("name", name)
                .field("base_url", base_url)
                .field("api_key", &"[REDACTED]")
                .field("models", models)
                .finish(),
            Self::DeleteProvider { provider_id } => formatter
                .debug_struct("DeleteProvider")
                .field("provider_id", provider_id)
                .finish(),
            Self::StartGlint => formatter.write_str("StartGlint"),
            Self::Exit => formatter.write_str("Exit"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupOutcome {
    StartGlint,
    Exit,
}

pub fn apply_setup_effect(
    manager: &mut ConfigurationManager,
    state: &mut SetupState,
    effect: SetupEffect,
) -> Result<Option<SetupOutcome>> {
    let result = match effect {
        SetupEffect::SaveBuiltin {
            provider_id,
            api_key,
        } => manager.save_builtin(&provider_id, api_key.as_deref()),
        SetupEffect::SaveCustom {
            name,
            base_url,
            api_key,
            models,
        } => manager.save_custom(&name, &base_url, api_key.as_deref(), models),
        SetupEffect::DeleteProvider { provider_id } => manager.delete_provider(&provider_id),
        SetupEffect::StartGlint => {
            let providers = match manager.available_providers() {
                Ok(providers) => providers,
                Err(_) => {
                    state.error = Some(
                        ConfigurationMutationErrorKind::CredentialUnavailable
                            .user_message()
                            .to_owned(),
                    );
                    return Ok(None);
                }
            };
            if providers.is_empty() {
                state.error = Some("Configure at least one model before starting Glint.".into());
                return Ok(None);
            }
            return Ok(Some(SetupOutcome::StartGlint));
        }
        SetupEffect::Exit => return Ok(Some(SetupOutcome::Exit)),
    };

    if let Err(error) = result {
        state.error = Some(error.user_message().to_owned());
        return Ok(None);
    }

    if state.refresh_provider_list(manager).is_err() {
        state.error = Some(
            ConfigurationMutationErrorKind::CredentialUnavailable
                .user_message()
                .to_owned(),
        );
        return Ok(None);
    }

    state.error = None;
    Ok(None)
}

fn edit_input(input: &mut InputState, action: KeyAction) {
    match action {
        KeyAction::Char(character) => input.push(character),
        KeyAction::Backspace => input.backspace(),
        KeyAction::Delete => input.delete_forward(),
        KeyAction::Left => input.move_left(),
        KeyAction::Right => input.move_right(),
        _ => {}
    }
}

fn nonblank_input(input: &InputState) -> Option<String> {
    (!input.value.trim().is_empty()).then(|| input.value.clone())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
    };

    use anyhow::{Result, bail};

    use super::*;
    use crate::{
        config::UserConfig,
        configuration::ConfigurationManager,
        credentials::{CredentialId, CredentialStore, CredentialStoreStatus},
        event::KeyAction,
        paths::GlintPaths,
        persistence::UserConfigRepository,
        provider_catalog::ProviderCatalog,
    };

    #[test]
    fn welcome_submit_opens_provider_list() {
        let catalog = test_catalog();
        let mut state = SetupState::welcome(&catalog);

        let effect = state.update(KeyAction::Submit);

        assert_eq!(effect, None);
        assert!(matches!(state.screen, SetupScreen::Providers(_)));
    }

    #[test]
    fn builtin_constructor_opens_the_requested_catalog_form() {
        let mut state = SetupState::builtin(test_catalog(), "deepseek");

        let form = state.builtin_form_mut().expect("built-in form");

        assert_eq!(form.provider_id, "deepseek");
        assert_eq!(form.models, ["deepseek-v4-flash", "deepseek-v4-pro"]);
    }

    #[test]
    fn custom_form_starts_with_one_unlabelled_model_row() {
        let form = CustomProviderForm::new();

        assert_eq!(form.models.len(), 1);
        assert_eq!(form.models[0].value, "");
    }

    #[test]
    fn add_and_delete_model_rows_preserve_one_row() {
        let mut form = CustomProviderForm::new();
        form.add_model_row();
        form.models[0].set("first");
        form.models[1].set("second");

        form.delete_model_row(0);
        assert_eq!(form.models.len(), 1);
        assert_eq!(form.models[0].value, "second");

        form.delete_model_row(0);
        assert_eq!(form.models.len(), 1);
        assert_eq!(form.models[0].value, "");
    }

    #[test]
    fn submit_on_keyboard_reachable_delete_target_clears_the_last_model_row() {
        let mut state = SetupState::custom_provider(test_catalog());
        state.custom_form_mut().unwrap().models[0].set("only-model");

        for _ in 0..4 {
            state.update(KeyAction::Tab);
        }
        state.update(KeyAction::Submit);

        let form = state.custom_form_mut().expect("custom form remains open");
        assert_eq!(form.models.len(), 1);
        assert_eq!(form.models[0].value, "");
        assert_eq!(form.focus, CustomFocus::Model(0));
    }

    #[test]
    fn delete_key_on_keyboard_reachable_delete_target_removes_that_model_row() {
        let mut state = SetupState::custom_provider(test_catalog());
        let form = state.custom_form_mut().unwrap();
        form.models[0].set("first");
        form.add_model_row();
        form.models[1].set("second");

        for _ in 0..4 {
            state.update(KeyAction::Tab);
        }
        state.update(KeyAction::Delete);

        let form = state.custom_form_mut().expect("custom form remains open");
        assert_eq!(form.models.len(), 1);
        assert_eq!(form.models[0].value, "second");
        assert_eq!(form.focus, CustomFocus::Model(0));
    }

    #[test]
    fn cancel_custom_form_emits_no_persistence_effect() {
        let mut state = SetupState::custom_provider(test_catalog());

        let effect = state.update(KeyAction::Escape);

        assert_eq!(effect, None);
        assert!(matches!(state.screen, SetupScreen::Providers(_)));
    }

    #[test]
    fn form_navigation_and_text_edits_are_deterministic() {
        let mut state = SetupState::custom_provider(test_catalog());

        state.update(KeyAction::Char('G'));
        state.update(KeyAction::Tab);
        state.update(KeyAction::Char('h'));
        state.update(KeyAction::Left);
        state.update(KeyAction::Delete);
        state.update(KeyAction::Tab);
        state.update(KeyAction::Char('k'));
        state.update(KeyAction::Tab);
        state.update(KeyAction::Char('m'));

        let form = state.custom_form_mut().expect("custom form");
        assert_eq!(form.name.value, "G");
        assert_eq!(form.base_url.value, "");
        assert_eq!(form.api_key.value, "k");
        assert_eq!(form.models[0].value, "m");
    }

    #[test]
    fn existing_custom_provider_keeps_identity_read_only_through_navigation_and_save() {
        let fixture = ManagerFixture::new();
        let mut manager = fixture.manager();
        manager
            .save_custom(
                "Gateway",
                "https://old.example/v1",
                Some("old-secret"),
                vec!["model".into()],
            )
            .unwrap();
        let mut state = SetupState::provider_list(&test_catalog(), &manager).unwrap();
        let custom_index = match &state.screen {
            SetupScreen::Providers(list) => list
                .rows
                .iter()
                .position(|row| row.provider_id() == Some("Gateway"))
                .unwrap(),
            _ => unreachable!(),
        };
        for _ in 0..custom_index {
            state.update(KeyAction::Down);
        }
        state.update(KeyAction::Submit);

        let form = state.custom_form_mut().expect("custom form");
        assert_eq!(form.name.value, "Gateway");
        assert_eq!(form.focus, CustomFocus::BaseUrl);

        state.update(KeyAction::Char('x'));
        for _ in 0..5 {
            state.update(KeyAction::Tab);
        }
        let effect = state.update(KeyAction::Submit).expect("save effect");
        assert!(matches!(
            &effect,
            SetupEffect::SaveCustom { name, .. } if name == "Gateway"
        ));
        apply_setup_effect(&mut manager, &mut state, effect).unwrap();

        assert_eq!(manager.user_config().custom_providers.len(), 1);
        assert!(
            manager
                .user_config()
                .custom_providers
                .contains_key("Gateway")
        );
        assert_eq!(
            manager.user_config().custom_providers["Gateway"].base_url,
            "https://old.example/v1x"
        );
    }

    #[test]
    fn setup_debug_output_redacts_api_keys() {
        let mut state = SetupState::custom_provider(test_catalog());
        let form = state.custom_form_mut().expect("custom form");
        form.api_key.set("very-secret-key");

        let state_debug = format!("{state:?}");
        let effect_debug = format!(
            "{:?}",
            SetupEffect::SaveCustom {
                name: "Gateway".into(),
                base_url: "https://llm.example/v1".into(),
                api_key: Some("very-secret-key".into()),
                models: vec!["model".into()],
            }
        );

        assert!(!state_debug.contains("very-secret-key"));
        assert!(!effect_debug.contains("very-secret-key"));
        assert!(state_debug.contains("[REDACTED]"));
        assert!(effect_debug.contains("[REDACTED]"));
    }

    #[test]
    fn failed_save_preserves_the_custom_draft_focus_and_redacts_error() {
        let fixture = ManagerFixture::new();
        fixture.repository.fail_saves();
        let mut manager = fixture.manager();
        let mut state = SetupState::custom_provider(test_catalog());
        let form = state.custom_form_mut().expect("custom form");
        form.name.set("Gateway");
        form.base_url.set("https://llm.example/v1");
        form.api_key.set("secret-value");
        form.models[0].set("model");
        form.focus = CustomFocus::Model(0);

        let outcome = apply_setup_effect(
            &mut manager,
            &mut state,
            SetupEffect::SaveCustom {
                name: "Gateway".into(),
                base_url: "https://llm.example/v1".into(),
                api_key: Some("secret-value".into()),
                models: vec!["model".into()],
            },
        )
        .expect("effect application");

        assert_eq!(outcome, None);
        let form = state.custom_form_mut().expect("custom form survives");
        assert_eq!(form.name.value, "Gateway");
        assert_eq!(form.base_url.value, "https://llm.example/v1");
        assert_eq!(form.api_key.value, "secret-value");
        assert_eq!(form.models[0].value, "model");
        assert_eq!(form.focus, CustomFocus::Model(0));
        let error = state.error.as_deref().expect("save error");
        assert!(!error.contains("secret-value"));
    }

    #[test]
    fn custom_validation_errors_are_actionable_and_distinct() {
        let cases = [
            (
                "",
                "https://llm.example/v1",
                vec!["model".into()],
                "Enter a provider name.",
            ),
            (
                "   ",
                "https://llm.example/v1",
                vec!["model".into()],
                "Enter a provider name.",
            ),
            (
                "DeepSeek",
                "https://llm.example/v1",
                vec!["model".into()],
                "Choose a unique provider name; it matches an existing provider.",
            ),
            (
                "Gateway",
                "not a URL",
                vec!["model".into()],
                "Enter a valid HTTP or HTTPS base URL.",
            ),
            (
                "Gateway",
                "https://llm.example/v1",
                vec![" ".into()],
                "Enter at least one model name.",
            ),
            (
                "Gateway",
                "https://llm.example/v1",
                vec!["model".into(), "model".into()],
                "Model names must be unique.",
            ),
        ];

        for (name, base_url, models, expected) in cases {
            let fixture = ManagerFixture::new();
            let mut manager = fixture.manager();
            let mut state = SetupState::custom_provider(test_catalog());
            let form = state.custom_form_mut().unwrap();
            form.name.set(name);
            form.base_url.set(base_url);
            form.models[0].set("draft-model");
            form.focus = CustomFocus::Model(0);

            apply_setup_effect(
                &mut manager,
                &mut state,
                SetupEffect::SaveCustom {
                    name: name.into(),
                    base_url: base_url.into(),
                    api_key: Some("sentinel-secret".into()),
                    models,
                },
            )
            .unwrap();

            assert_eq!(state.error.as_deref(), Some(expected));
            let form = state.custom_form_mut().expect("draft remains visible");
            assert_eq!(form.focus, CustomFocus::Model(0));
            assert_eq!(form.api_key.value, "");
            assert!(!state.error.as_deref().unwrap().contains("sentinel-secret"));
        }
    }

    #[test]
    fn credential_and_filesystem_failures_have_safe_actionable_messages() {
        let credential_fixture = ManagerFixture::new();
        credential_fixture.credentials.fail_get_after(0);
        let mut credential_manager = credential_fixture.manager();
        let mut credential_state = SetupState::builtin(test_catalog(), "deepseek");
        let form = credential_state.builtin_form_mut().unwrap();
        form.api_key.set("sentinel-secret");
        form.focus = BuiltinFocus::Save;

        apply_setup_effect(
            &mut credential_manager,
            &mut credential_state,
            SetupEffect::SaveBuiltin {
                provider_id: "deepseek".into(),
                api_key: Some("sentinel-secret".into()),
            },
        )
        .unwrap();

        let credential_error = credential_state.error.as_deref().unwrap();
        assert!(credential_error.contains("Credential storage is unavailable"));
        assert!(!credential_error.contains("sentinel-secret"));
        let form = credential_state.builtin_form_mut().unwrap();
        assert_eq!(form.api_key.value, "sentinel-secret");
        assert_eq!(form.focus, BuiltinFocus::Save);

        let persistence_fixture = ManagerFixture::new();
        persistence_fixture.repository.fail_saves();
        let mut persistence_manager = persistence_fixture.manager();
        let mut persistence_state = SetupState::builtin(test_catalog(), "deepseek");
        let form = persistence_state.builtin_form_mut().unwrap();
        form.api_key.set("sentinel-secret");
        form.focus = BuiltinFocus::Save;

        apply_setup_effect(
            &mut persistence_manager,
            &mut persistence_state,
            SetupEffect::SaveBuiltin {
                provider_id: "deepseek".into(),
                api_key: Some("sentinel-secret".into()),
            },
        )
        .unwrap();

        let persistence_error = persistence_state.error.as_deref().unwrap();
        assert!(persistence_error.contains("Check that ~/.glint is writable"));
        assert!(!persistence_error.contains("sentinel-secret"));
        let form = persistence_state.builtin_form_mut().unwrap();
        assert_eq!(form.api_key.value, "sentinel-secret");
        assert_eq!(form.focus, BuiltinFocus::Save);
    }

    #[test]
    fn provider_setup_exposes_safe_degraded_credential_repair_notice() {
        let fixture = ManagerFixture::new();
        let mut user = UserConfig::default();
        user.configured_providers.push("deepseek".into());
        fixture.repository.replace(user);
        fixture.credentials.mark_keyring_unavailable();
        let manager = fixture.manager();

        let state = SetupState::provider_list(&test_catalog(), &manager).unwrap();

        let notice = state.notice.as_deref().expect("degraded-store notice");
        assert_eq!(
            notice,
            "The system credential store is unavailable.\nRe-enter an API key to repair this provider.\nGlint will switch to its protected auth.json file."
        );
        assert!(!notice.contains("sentinel-secret"));
    }

    #[test]
    fn refresh_failure_after_a_successful_save_stays_redacted_and_keeps_the_form() {
        let fixture = ManagerFixture::new();
        fixture.credentials.fail_get_after(2);
        let mut manager = fixture.manager();
        let mut state = SetupState::builtin(test_catalog(), "deepseek");
        let form = state.builtin_form_mut().expect("built-in form");
        form.api_key.set("secret-value");
        form.focus = BuiltinFocus::Save;

        let outcome = apply_setup_effect(
            &mut manager,
            &mut state,
            SetupEffect::SaveBuiltin {
                provider_id: "deepseek".into(),
                api_key: Some("secret-value".into()),
            },
        )
        .expect("refresh failures become state errors");

        assert_eq!(outcome, None);
        assert_eq!(manager.user_config().configured_providers, ["deepseek"]);
        let form = state.builtin_form_mut().expect("form stays visible");
        assert_eq!(form.api_key.value, "secret-value");
        assert_eq!(form.focus, BuiltinFocus::Save);
        assert!(
            state
                .error
                .as_deref()
                .expect("redacted refresh error")
                .contains("Credential storage is unavailable")
        );
        assert!(!state.error.as_deref().unwrap().contains("secret-value"));
    }

    #[test]
    fn blank_existing_builtin_key_is_retained_by_the_manager_effect() {
        let fixture = ManagerFixture::new();
        let credentials = fixture.credentials.clone();
        let mut manager = fixture.manager();
        manager
            .save_builtin("deepseek", Some("old-secret"))
            .unwrap();
        let mut state = SetupState::welcome(&test_catalog());

        apply_setup_effect(
            &mut manager,
            &mut state,
            SetupEffect::SaveBuiltin {
                provider_id: "deepseek".into(),
                api_key: None,
            },
        )
        .unwrap();

        assert_eq!(
            credentials.get(&CredentialId::builtin("deepseek")).unwrap(),
            Some("old-secret".into())
        );
    }

    #[test]
    fn deletion_requires_confirmation_before_emitting_effect() {
        let fixture = ManagerFixture::new();
        let mut manager = fixture.manager();
        manager.save_builtin("deepseek", Some("key")).unwrap();
        let mut state = SetupState::provider_list(&test_catalog(), &manager).unwrap();

        let first = state.update(KeyAction::Delete);
        assert_eq!(first, None);
        assert!(matches!(state.screen, SetupScreen::ConfirmDelete(_)));

        let second = state.update(KeyAction::Submit);
        assert_eq!(
            second,
            Some(SetupEffect::DeleteProvider {
                provider_id: "deepseek".into()
            })
        );
    }

    #[test]
    fn start_glint_is_not_emitted_without_available_models() {
        let mut state = SetupState::welcome(&test_catalog());
        state.update(KeyAction::Submit);
        for _ in 0..20 {
            let effect = state.update(KeyAction::Submit);
            assert_ne!(effect, Some(SetupEffect::StartGlint));
            state.update(KeyAction::Down);
        }
    }

    fn test_catalog() -> ProviderCatalog {
        ProviderCatalog::embedded().expect("embedded catalog")
    }

    struct ManagerFixture {
        credentials: MemoryCredentialStore,
        repository: MemoryUserConfigStore,
    }

    impl ManagerFixture {
        fn new() -> Self {
            Self {
                credentials: MemoryCredentialStore::default(),
                repository: MemoryUserConfigStore {
                    path: PathBuf::from("/fixture/.glint/config.yaml"),
                    ..MemoryUserConfigStore::default()
                },
            }
        }

        fn manager(&self) -> ConfigurationManager {
            ConfigurationManager::new(
                GlintPaths::from_home("/fixture"),
                PathBuf::from("/workspace"),
                test_catalog(),
                Box::new(self.repository.clone()),
                Box::new(self.credentials.clone()),
            )
            .expect("configuration manager")
        }
    }

    #[derive(Clone, Default)]
    struct MemoryUserConfigStore {
        config: Arc<Mutex<Option<UserConfig>>>,
        fail_save: Arc<Mutex<bool>>,
        path: PathBuf,
    }

    impl MemoryUserConfigStore {
        fn replace(&self, config: UserConfig) {
            *self.config.lock().expect("config lock") = Some(config);
        }

        fn fail_saves(&self) {
            *self.fail_save.lock().expect("save lock") = true;
        }
    }

    impl UserConfigRepository for MemoryUserConfigStore {
        fn path(&self) -> &Path {
            &self.path
        }

        fn load(&self) -> Result<Option<UserConfig>> {
            Ok(self.config.lock().expect("config lock").clone())
        }

        fn save(&self, config: &UserConfig) -> Result<()> {
            if *self.fail_save.lock().expect("save lock") {
                bail!("injected save failed for secret-value")
            }
            *self.config.lock().expect("config lock") = Some(config.clone());
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct MemoryCredentialStore {
        values: Arc<Mutex<BTreeMap<String, String>>>,
        get_calls: Arc<std::sync::atomic::AtomicUsize>,
        fail_get_after: Arc<Mutex<Option<usize>>>,
        keyring_unavailable: Arc<Mutex<bool>>,
    }

    impl MemoryCredentialStore {
        fn fail_get_after(&self, successful_gets: usize) {
            *self.fail_get_after.lock().expect("failure lock") = Some(successful_gets);
        }

        fn mark_keyring_unavailable(&self) {
            *self.keyring_unavailable.lock().expect("status lock") = true;
        }
    }

    impl CredentialStore for MemoryCredentialStore {
        fn get(&self, id: &CredentialId) -> Result<Option<String>> {
            let call = self
                .get_calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1;
            if self
                .fail_get_after
                .lock()
                .expect("failure lock")
                .is_some_and(|successful_gets| call > successful_gets)
            {
                bail!("injected credential refresh failure for secret-value")
            }
            Ok(self
                .values
                .lock()
                .expect("credentials lock")
                .get(id.as_str())
                .cloned())
        }

        fn set(&self, id: &CredentialId, api_key: &str) -> Result<()> {
            self.values
                .lock()
                .expect("credentials lock")
                .insert(id.as_str().to_owned(), api_key.to_owned());
            Ok(())
        }

        fn delete(&self, id: &CredentialId) -> Result<()> {
            self.values
                .lock()
                .expect("credentials lock")
                .remove(id.as_str());
            Ok(())
        }

        fn status(&self) -> CredentialStoreStatus {
            if *self.keyring_unavailable.lock().expect("status lock") {
                CredentialStoreStatus::KeyringUnavailable {
                    diagnostic: "safe status; backend details sentinel-secret".into(),
                }
            } else {
                CredentialStoreStatus::Ready
            }
        }
    }
}
