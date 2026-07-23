use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    str::FromStr,
    sync::{Arc, Mutex, RwLock, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use http::{HeaderName, HeaderValue};
use rmcp::{
    ClientHandler, RoleClient, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResult, ClientCapabilities, ClientInfo,
        CreateElicitationRequestParams, CreateElicitationResult, ElicitationAction,
        GetPromptRequestParams, Implementation, ListRootsResult, ReadResourceRequestParams, Root,
        SubscribeRequestParams, UnsubscribeRequestParams,
    },
    service::{NotificationContext, Peer, RequestContext, RunningService},
    transport::{
        StreamableHttpClientTransport, TokioChildProcess,
        auth::{AuthClient, AuthError, CredentialStore, OAuthState, StoredCredentials},
    },
};
use serde_json::{Map, Value, json};
use tokio::{
    io::AsyncReadExt,
    process::Command,
    sync::{mpsc as tokio_mpsc, oneshot},
    time,
};

use crate::{
    agent::provider::{ToolCall, ToolResult, ToolSpec},
    tools::{DynamicTool, sanitize_tool_name},
};

use super::{McpApprovalPolicy, McpConfig, McpServerConfig, McpTransportConfig};

type McpService = RunningService<RoleClient, GlintClientHandler>;
type McpOAuthClient = AuthClient<reqwest::Client>;

#[derive(Clone)]
struct FileCredentialStore {
    path: PathBuf,
}

impl FileCredentialStore {
    fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

#[async_trait::async_trait]
impl CredentialStore for FileCredentialStore {
    async fn load(&self) -> std::result::Result<Option<StoredCredentials>, AuthError> {
        match fs::read(&self.path) {
            Ok(contents) => serde_json::from_slice(&contents)
                .map(Some)
                .map_err(auth_storage_error),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(auth_storage_error(error)),
        }
    }

    async fn save(&self, credentials: StoredCredentials) -> std::result::Result<(), AuthError> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| auth_storage_error("OAuth credential path has no parent"))?;
        fs::create_dir_all(parent).map_err(auth_storage_error)?;
        set_private_dir_permissions(parent)?;

        let temporary = self
            .path
            .with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
        let contents = serde_json::to_vec(&credentials).map_err(auth_storage_error)?;
        write_private_file(&temporary, &contents)?;
        #[cfg(windows)]
        if self.path.exists() {
            fs::remove_file(&self.path).map_err(auth_storage_error)?;
        }
        fs::rename(&temporary, &self.path).map_err(auth_storage_error)
    }

    async fn clear(&self) -> std::result::Result<(), AuthError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(auth_storage_error(error)),
        }
    }
}

#[derive(Clone)]
pub struct McpManager {
    commands: tokio_mpsc::UnboundedSender<ManagerCommand>,
    state: Arc<RwLock<ManagerState>>,
    elicitations: Arc<Mutex<mpsc::Receiver<McpElicitation>>>,
}

pub struct McpElicitation {
    pub id: u64,
    pub request: McpElicitationRequest,
    response: oneshot::Sender<CreateElicitationResult>,
}

pub enum McpElicitationRequest {
    Form {
        message: String,
        schema: Value,
    },
    Url {
        message: String,
        url: String,
        elicitation_id: String,
    },
}

impl McpElicitation {
    pub fn respond(self, accepted: bool, content: Option<Value>) {
        let action = if accepted {
            ElicitationAction::Accept
        } else {
            ElicitationAction::Decline
        };
        let mut result = CreateElicitationResult::new(action);
        result.content = content;
        self.response.send(result).ok();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerStatus {
    pub name: String,
    pub state: McpConnectionState,
    pub tools: Vec<McpToolStatus>,
    pub resources: Vec<McpCapabilityStatus>,
    pub resource_templates: Vec<McpCapabilityStatus>,
    pub prompts: Vec<McpCapabilityStatus>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpToolStatus {
    pub name: String,
    pub description: String,
    pub approval: McpApprovalPolicy,
    pub read_only: bool,
    pub timeout_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpCapabilityStatus {
    pub name: String,
    pub detail: Option<String>,
    pub description: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpConnectionState {
    Starting,
    Ready,
    Failed,
    Stopped,
}

#[derive(Default)]
struct ManagerState {
    servers: BTreeMap<String, McpServerStatus>,
    tools: Vec<McpToolDefinition>,
}

#[derive(Clone)]
struct McpToolDefinition {
    exposed_name: String,
    description: String,
    parameters: Value,
    server: String,
    operation: McpOperation,
    approval: McpApprovalPolicy,
    concurrency_safe: bool,
    timeout: Duration,
}

#[derive(Clone)]
enum McpOperation {
    CallTool { name: String },
    ListResources,
    ListResourceTemplates,
    ReadResource,
    SubscribeResource,
    UnsubscribeResource,
    ListPrompts,
    GetPrompt,
}

enum ManagerCommand {
    Execute {
        id: String,
        server: String,
        operation: McpOperation,
        arguments: Value,
        timeout: Duration,
        response: mpsc::Sender<Result<McpResponse>>,
    },
    Cancel {
        id: String,
    },
    Reconnect {
        server: String,
        response: mpsc::Sender<Result<()>>,
    },
    BeginOAuth {
        server: String,
        response: mpsc::Sender<Result<String>>,
    },
    CompleteOAuth {
        server: String,
        callback_url: String,
        response: mpsc::Sender<Result<()>>,
    },
    LogoutOAuth {
        server: String,
        response: mpsc::Sender<Result<()>>,
    },
    Shutdown,
}

enum NotificationEvent {
    Tools(String),
    Resources(String),
    Prompts(String),
}

struct McpResponse {
    content: String,
    is_error: bool,
}

#[derive(Clone)]
struct GlintClientHandler {
    info: ClientInfo,
    server: String,
    notifications: tokio_mpsc::UnboundedSender<NotificationEvent>,
    root: String,
    elicitations: mpsc::Sender<McpElicitation>,
}

impl ClientHandler for GlintClientHandler {
    fn get_info(&self) -> ClientInfo {
        self.info.clone()
    }

    async fn list_roots(
        &self,
        _context: RequestContext<RoleClient>,
    ) -> std::result::Result<ListRootsResult, rmcp::ErrorData> {
        Ok(ListRootsResult::new(vec![
            Root::new(self.root.clone()).with_name("Glint workspace"),
        ]))
    }

    async fn create_elicitation(
        &self,
        request: CreateElicitationRequestParams,
        _context: RequestContext<RoleClient>,
    ) -> std::result::Result<CreateElicitationResult, rmcp::ErrorData> {
        static NEXT_ID: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(1_u64 << 63);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let request = match request {
            CreateElicitationRequestParams::FormElicitationParams {
                message,
                requested_schema,
                ..
            } => McpElicitationRequest::Form {
                message,
                schema: serde_json::to_value(requested_schema)
                    .map_err(|error| rmcp::ErrorData::internal_error(error.to_string(), None))?,
            },
            CreateElicitationRequestParams::UrlElicitationParams {
                message,
                url,
                elicitation_id,
                ..
            } => McpElicitationRequest::Url {
                message,
                url,
                elicitation_id,
            },
        };
        let (response, receive_response) = oneshot::channel();
        self.elicitations
            .send(McpElicitation {
                id,
                request,
                response,
            })
            .map_err(|_| rmcp::ErrorData::internal_error("Glint UI is unavailable", None))?;
        receive_response
            .await
            .map_err(|_| rmcp::ErrorData::internal_error("elicitation was cancelled", None))
    }

    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.notifications
            .send(NotificationEvent::Tools(self.server.clone()))
            .ok();
    }

    async fn on_resource_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.notifications
            .send(NotificationEvent::Resources(self.server.clone()))
            .ok();
    }

    async fn on_prompt_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.notifications
            .send(NotificationEvent::Prompts(self.server.clone()))
            .ok();
    }
}

impl McpManager {
    pub fn new(config: McpConfig, cwd: PathBuf) -> Self {
        Self::start(config, cwd, true)
    }

    pub fn new_background(config: McpConfig, cwd: PathBuf) -> Self {
        Self::start(config, cwd, false)
    }

    fn start(config: McpConfig, cwd: PathBuf, wait_until_ready: bool) -> Self {
        let state = Arc::new(RwLock::new(ManagerState::default()));
        initialize_statuses(&state, &config);
        let (commands, command_rx) = tokio_mpsc::unbounded_channel();
        let (elicitation_tx, elicitation_rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let worker_state = Arc::clone(&state);
        thread::Builder::new()
            .name("glint-mcp".to_owned())
            .spawn(move || {
                run_worker(
                    config,
                    cwd,
                    command_rx,
                    worker_state,
                    ready_tx,
                    elicitation_tx,
                )
            })
            .expect("failed to spawn MCP runtime thread");
        if wait_until_ready {
            ready_rx.recv_timeout(Duration::from_secs(60)).ok();
        }
        Self {
            commands,
            state,
            elicitations: Arc::new(Mutex::new(elicitation_rx)),
        }
    }

    pub fn dynamic_tools(&self) -> Vec<Arc<dyn DynamicTool>> {
        self.state
            .read()
            .expect("MCP state lock poisoned")
            .tools
            .iter()
            .cloned()
            .map(|definition| {
                Arc::new(McpDynamicTool {
                    manager: self.clone(),
                    definition,
                }) as Arc<dyn DynamicTool>
            })
            .collect()
    }

    pub fn statuses(&self) -> Vec<McpServerStatus> {
        self.state
            .read()
            .expect("MCP state lock poisoned")
            .servers
            .values()
            .cloned()
            .collect()
    }

    pub fn try_recv_elicitation(&self) -> Option<McpElicitation> {
        self.elicitations
            .lock()
            .expect("MCP elicitation lock poisoned")
            .try_recv()
            .ok()
    }

    pub fn status_text(&self) -> String {
        let statuses = self.statuses();
        if statuses.is_empty() {
            return "No MCP servers configured.".to_owned();
        }
        statuses
            .into_iter()
            .map(|status| {
                let state = match status.state {
                    McpConnectionState::Starting => "starting",
                    McpConnectionState::Ready => "ready",
                    McpConnectionState::Failed => "failed",
                    McpConnectionState::Stopped => "stopped",
                };
                let error = status
                    .error
                    .map(|error| format!(" — {error}"))
                    .unwrap_or_default();
                format!(
                    "{}  {}  {} tools, {} resources, {} prompts{}",
                    status.name,
                    state,
                    status.tools.len(),
                    status.resources.len() + status.resource_templates.len(),
                    status.prompts.len(),
                    error
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn reconnect(&self, server: &str) -> Result<()> {
        let (response_tx, response_rx) = mpsc::channel();
        self.commands
            .send(ManagerCommand::Reconnect {
                server: server.to_owned(),
                response: response_tx,
            })
            .map_err(|_| anyhow!("MCP runtime is stopped"))?;
        response_rx
            .recv_timeout(Duration::from_secs(60))
            .context("timed out reconnecting MCP server")?
    }

    pub fn begin_oauth(&self, server: &str) -> Result<String> {
        let (response_tx, response_rx) = mpsc::channel();
        self.commands
            .send(ManagerCommand::BeginOAuth {
                server: server.to_owned(),
                response: response_tx,
            })
            .map_err(|_| anyhow!("MCP runtime is stopped"))?;
        response_rx
            .recv_timeout(Duration::from_secs(60))
            .context("timed out starting MCP OAuth authorization")?
    }

    pub fn complete_oauth(&self, server: &str, callback_url: &str) -> Result<()> {
        let (response_tx, response_rx) = mpsc::channel();
        self.commands
            .send(ManagerCommand::CompleteOAuth {
                server: server.to_owned(),
                callback_url: callback_url.to_owned(),
                response: response_tx,
            })
            .map_err(|_| anyhow!("MCP runtime is stopped"))?;
        response_rx
            .recv_timeout(Duration::from_secs(60))
            .context("timed out completing MCP OAuth authorization")?
    }

    pub fn logout_oauth(&self, server: &str) -> Result<()> {
        let (response_tx, response_rx) = mpsc::channel();
        self.commands
            .send(ManagerCommand::LogoutOAuth {
                server: server.to_owned(),
                response: response_tx,
            })
            .map_err(|_| anyhow!("MCP runtime is stopped"))?;
        response_rx
            .recv_timeout(Duration::from_secs(60))
            .context("timed out clearing MCP OAuth credentials")?
    }

    pub fn shutdown(&self) {
        self.commands.send(ManagerCommand::Shutdown).ok();
    }

    fn execute(
        &self,
        definition: &McpToolDefinition,
        call: &ToolCall,
        is_cancelled: &mut dyn FnMut() -> bool,
    ) -> ToolResult {
        if definition.approval == McpApprovalPolicy::Deny {
            return tool_error(call, "MCP tool is denied by configuration".to_owned());
        }
        let (response_tx, response_rx) = mpsc::channel();
        let operation_id = uuid::Uuid::new_v4().to_string();
        if self
            .commands
            .send(ManagerCommand::Execute {
                id: operation_id.clone(),
                server: definition.server.clone(),
                operation: definition.operation.clone(),
                arguments: call.arguments.clone(),
                timeout: definition.timeout,
                response: response_tx,
            })
            .is_err()
        {
            return tool_error(call, "MCP runtime is stopped".to_owned());
        }
        loop {
            if is_cancelled() {
                self.commands
                    .send(ManagerCommand::Cancel { id: operation_id })
                    .ok();
                return tool_error(call, "MCP tool call cancelled".to_owned());
            }
            match response_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(response)) => {
                    return ToolResult {
                        call_id: call.id.clone(),
                        content: response.content,
                        is_error: response.is_error,
                    };
                }
                Ok(Err(error)) => return tool_error(call, format!("MCP error: {error:#}")),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return tool_error(call, "MCP response channel closed".to_owned());
                }
            }
        }
    }
}

struct McpDynamicTool {
    manager: McpManager,
    definition: McpToolDefinition,
}

impl DynamicTool for McpDynamicTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.definition.exposed_name.clone(),
            description: self.definition.description.clone(),
            parameters: self.definition.parameters.clone(),
        }
    }

    fn execute(&self, call: &ToolCall, is_cancelled: &mut dyn FnMut() -> bool) -> ToolResult {
        self.manager.execute(&self.definition, call, is_cancelled)
    }

    fn requires_approval(&self, _call: &ToolCall) -> bool {
        self.definition.approval == McpApprovalPolicy::Prompt
    }

    fn is_concurrency_safe(&self, _call: &ToolCall) -> bool {
        self.definition.concurrency_safe
    }

    fn input_summary(&self, call: &ToolCall) -> String {
        format!("{} {}", self.definition.server, call.arguments)
    }

    fn input_description(&self, _call: &ToolCall) -> Option<String> {
        Some(format!("MCP server: {}", self.definition.server))
    }
}

fn run_worker(
    config: McpConfig,
    cwd: PathBuf,
    command_rx: tokio_mpsc::UnboundedReceiver<ManagerCommand>,
    state: Arc<RwLock<ManagerState>>,
    ready: mpsc::Sender<()>,
    elicitations: mpsc::Sender<McpElicitation>,
) {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("glint-mcp-io")
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let mut state = state.write().expect("MCP state lock poisoned");
            for name in config.servers.keys() {
                state.servers.insert(
                    name.clone(),
                    failed_status(name, format!("failed to create MCP runtime: {error}")),
                );
            }
            ready.send(()).ok();
            return;
        }
    };
    runtime.block_on(worker_loop(
        config,
        cwd,
        command_rx,
        state,
        ready,
        elicitations,
    ));
}

async fn worker_loop(
    config: McpConfig,
    cwd: PathBuf,
    mut commands: tokio_mpsc::UnboundedReceiver<ManagerCommand>,
    state: Arc<RwLock<ManagerState>>,
    ready: mpsc::Sender<()>,
    elicitations: mpsc::Sender<McpElicitation>,
) {
    let (notification_tx, mut notifications) = tokio_mpsc::unbounded_channel();
    let mut services = BTreeMap::new();
    let mut in_flight = BTreeMap::new();
    let mut oauth_states = BTreeMap::new();
    let mut oauth_clients = BTreeMap::new();
    for (name, server) in config.servers.iter().filter(|(_, server)| server.enabled) {
        let oauth_client = if server_oauth(server).is_some() {
            match restore_oauth_client(name, server, &cwd).await {
                Ok(Some(client)) => {
                    oauth_clients.insert(name.clone(), client.clone());
                    Some(client)
                }
                Ok(None) => {
                    set_failed(
                        &state,
                        name,
                        anyhow!("OAuth authorization required; run `/mcp auth {name}`"),
                    );
                    continue;
                }
                Err(error) => {
                    set_failed(&state, name, error);
                    continue;
                }
            }
        } else {
            None
        };
        match connect_server(
            name,
            server,
            &cwd,
            notification_tx.clone(),
            elicitations.clone(),
            oauth_client,
        )
        .await
        {
            Ok(service) => {
                services.insert(name.clone(), service);
                refresh_server(name, server, &services, &state).await;
            }
            Err(error) => set_failed(&state, name, error),
        }
    }
    ready.send(()).ok();

    loop {
        tokio::select! {
            Some(command) = commands.recv() => match command {
                ManagerCommand::Execute { id, server, operation, arguments, timeout, response } => {
                    in_flight.retain(|_, task: &mut tokio::task::JoinHandle<()>| !task.is_finished());
                    let Some(service) = services.get(&server) else {
                        response.send(Err(anyhow!("MCP server '{server}' is not connected"))).ok();
                        continue;
                    };
                    let peer = service.peer().clone();
                    let operation_server = server.clone();
                    let task = tokio::spawn(async move {
                        let result = execute_operation(peer, &operation_server, operation, arguments, timeout).await;
                        response.send(result).ok();
                    });
                    in_flight.insert(id, task);
                }
                ManagerCommand::Cancel { id } => {
                    if let Some(task) = in_flight.remove(&id) {
                        task.abort();
                    }
                }
                ManagerCommand::Reconnect { server, response } => {
                    let result = reconnect_server(
                        &server,
                        &config,
                        &cwd,
                        notification_tx.clone(),
                        elicitations.clone(),
                        oauth_clients.get(&server).cloned(),
                        &mut services,
                        &state,
                    ).await;
                    response.send(result).ok();
                }
                ManagerCommand::BeginOAuth { server, response } => {
                    let result = begin_oauth_authorization(
                        &server,
                        &config,
                        &cwd,
                        &mut oauth_states,
                    ).await;
                    response.send(result).ok();
                }
                ManagerCommand::CompleteOAuth { server, callback_url, response } => {
                    let result = complete_oauth_authorization(
                        &server,
                        &callback_url,
                        &config,
                        &cwd,
                        notification_tx.clone(),
                        elicitations.clone(),
                        &mut oauth_states,
                        &mut oauth_clients,
                        &mut services,
                        &state,
                    ).await;
                    response.send(result).ok();
                }
                ManagerCommand::LogoutOAuth { server, response } => {
                    let result = logout_oauth(
                        &server,
                        &config,
                        &cwd,
                        &mut oauth_states,
                        &mut oauth_clients,
                        &mut services,
                        &state,
                    ).await;
                    response.send(result).ok();
                }
                ManagerCommand::Shutdown => {
                    for (_, task) in std::mem::take(&mut in_flight) {
                        task.abort();
                    }
                    break;
                },
            },
            Some(notification) = notifications.recv() => {
                let server = match notification {
                    NotificationEvent::Tools(server)
                    | NotificationEvent::Resources(server)
                    | NotificationEvent::Prompts(server) => server,
                };
                if let Some(server_config) = config.servers.get(&server) {
                    refresh_server(&server, server_config, &services, &state).await;
                }
            }
            else => break,
        }
    }

    for (name, mut service) in services {
        let _ = service.close_with_timeout(Duration::from_secs(2)).await;
        if let Some(status) = state
            .write()
            .expect("MCP state lock poisoned")
            .servers
            .get_mut(&name)
        {
            status.state = McpConnectionState::Stopped;
        }
    }
}

fn initialize_statuses(state: &Arc<RwLock<ManagerState>>, config: &McpConfig) {
    let mut state = state.write().expect("MCP state lock poisoned");
    for (name, server) in &config.servers {
        state.servers.insert(
            name.clone(),
            McpServerStatus {
                name: name.clone(),
                state: if server.enabled {
                    McpConnectionState::Starting
                } else {
                    McpConnectionState::Stopped
                },
                tools: Vec::new(),
                resources: Vec::new(),
                resource_templates: Vec::new(),
                prompts: Vec::new(),
                error: None,
            },
        );
    }
}

async fn connect_server(
    name: &str,
    config: &McpServerConfig,
    fallback_cwd: &Path,
    notifications: tokio_mpsc::UnboundedSender<NotificationEvent>,
    elicitations: mpsc::Sender<McpElicitation>,
    oauth_client: Option<McpOAuthClient>,
) -> Result<McpService> {
    let handler = GlintClientHandler {
        info: ClientInfo::new(
            client_capabilities(),
            Implementation::new("glint", env!("CARGO_PKG_VERSION")).with_title("Glint"),
        ),
        server: name.to_owned(),
        notifications,
        root: reqwest::Url::from_directory_path(fallback_cwd)
            .map(|url| url.to_string())
            .unwrap_or_else(|_| format!("file://{}", fallback_cwd.display())),
        elicitations,
    };
    let connect = async {
        match &config.transport {
            McpTransportConfig::Stdio {
                command,
                args,
                env,
                env_vars,
                cwd,
            } => {
                let mut process = Command::new(command);
                process
                    .args(args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .current_dir(resolve_server_cwd(cwd.as_deref(), fallback_cwd));
                for (key, value) in env {
                    process.env(key, value);
                }
                for key in env_vars {
                    let value = std::env::var(key).with_context(|| {
                        format!("required environment variable '{key}' is missing")
                    })?;
                    process.env(key, value);
                }
                let (transport, stderr) = TokioChildProcess::builder(process)
                    .stderr(Stdio::piped())
                    .spawn()
                    .with_context(|| format!("failed to start MCP server '{name}'"))?;
                if let Some(mut stderr) = stderr {
                    let _stderr_drain = tokio::spawn(async move {
                        let mut buffer = [0_u8; 8192];
                        loop {
                            match stderr.read(&mut buffer).await {
                                Ok(0) | Err(_) => break,
                                Ok(_) => {}
                            }
                        }
                    });
                }
                handler.clone().serve(transport).await.map_err(Into::into)
            }
            McpTransportConfig::StreamableHttp {
                url,
                headers,
                bearer_token_env,
                oauth,
            } => {
                if bearer_token_env.is_some() && oauth.is_some() {
                    bail!("MCP server '{name}' cannot configure both bearer_token_env and oauth");
                }
                let mut custom_headers = std::collections::HashMap::new();
                for (key, value) in headers {
                    custom_headers.insert(
                        HeaderName::from_str(key)
                            .with_context(|| format!("invalid MCP HTTP header name '{key}'"))?,
                        HeaderValue::from_str(value).with_context(|| {
                            format!("invalid MCP HTTP header value for '{key}'")
                        })?,
                    );
                }
                let mut transport_config =
                    rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig::with_uri(url.clone())
                        .custom_headers(custom_headers)
                        .reinit_on_expired_session(true);
                if let Some(token_env) = bearer_token_env {
                    transport_config = transport_config.auth_header(
                        std::env::var(token_env).with_context(|| {
                            format!("required bearer token environment variable '{token_env}' is missing")
                        })?,
                    );
                }
                if oauth.is_some() {
                    let client = oauth_client.with_context(|| {
                        format!("OAuth authorization required; run `/mcp auth {name}`")
                    })?;
                    let transport =
                        StreamableHttpClientTransport::with_client(client, transport_config);
                    handler.serve(transport).await.map_err(Into::into)
                } else {
                    let transport = StreamableHttpClientTransport::from_config(transport_config);
                    handler.serve(transport).await.map_err(Into::into)
                }
            }
        }
    };
    time::timeout(Duration::from_millis(config.startup_timeout_ms), connect)
        .await
        .with_context(|| format!("MCP server '{name}' startup timed out"))?
}

#[allow(deprecated)]
fn client_capabilities() -> ClientCapabilities {
    ClientCapabilities::builder()
        .enable_roots()
        .enable_elicitation()
        .build()
}

async fn refresh_server(
    name: &str,
    config: &McpServerConfig,
    services: &BTreeMap<String, McpService>,
    state: &Arc<RwLock<ManagerState>>,
) {
    let Some(service) = services.get(name) else {
        return;
    };
    let discovery = async {
        tokio::join!(
            service.list_all_tools(),
            service.list_all_resources(),
            service.list_all_resource_templates(),
            service.list_all_prompts(),
        )
    };
    let (tools, resources, templates, prompts) =
        match time::timeout(Duration::from_millis(config.tool_timeout_ms), discovery).await {
            Ok(discovery) => discovery,
            Err(_) => {
                set_failed(state, name, anyhow!("MCP capability discovery timed out"));
                return;
            }
        };
    let tools = tools.unwrap_or_default();
    let resources = resources.unwrap_or_default();
    let templates = templates.unwrap_or_default();
    let prompts = prompts.unwrap_or_default();
    let mut definitions = tools
        .into_iter()
        .filter(|tool| config.tool_enabled(tool.name.as_ref()))
        .map(|tool| {
            let remote_name = tool.name.into_owned();
            let approval = config.approval_for_tool(&remote_name);
            let read_only = tool
                .annotations
                .as_ref()
                .and_then(|annotations| annotations.read_only_hint)
                .unwrap_or(false);
            McpToolDefinition {
                exposed_name: exposed_tool_name(name, &remote_name),
                description: tool
                    .description
                    .map(|description| description.into_owned())
                    .unwrap_or_else(|| format!("MCP tool {remote_name} from server {name}")),
                parameters: Value::Object((*tool.input_schema).clone()),
                server: name.to_owned(),
                operation: McpOperation::CallTool { name: remote_name },
                approval,
                concurrency_safe: read_only,
                timeout: Duration::from_millis(config.tool_timeout_ms),
            }
        })
        .collect::<Vec<_>>();
    if !resources.is_empty() || !templates.is_empty() {
        definitions.extend(resource_tools(name, config));
    }
    if !prompts.is_empty() {
        definitions.extend(prompt_tools(name, config));
    }
    let tool_statuses = definitions
        .iter()
        .map(|definition| McpToolStatus {
            name: definition.exposed_name.clone(),
            description: definition.description.clone(),
            approval: definition.approval,
            read_only: definition.concurrency_safe,
            timeout_ms: definition.timeout.as_millis() as u64,
        })
        .collect::<Vec<_>>();
    let resource_statuses = resources
        .iter()
        .map(|resource| McpCapabilityStatus {
            name: resource.name.clone(),
            detail: Some(resource.uri.clone()),
            description: resource.description.clone(),
        })
        .collect::<Vec<_>>();
    let template_statuses = templates
        .iter()
        .map(|template| McpCapabilityStatus {
            name: template.name.clone(),
            detail: Some(template.uri_template.clone()),
            description: template.description.clone(),
        })
        .collect::<Vec<_>>();
    let prompt_statuses = prompts
        .iter()
        .map(|prompt| McpCapabilityStatus {
            name: prompt.name.clone(),
            detail: prompt.arguments.as_ref().map(|arguments| {
                arguments
                    .iter()
                    .map(|argument| {
                        if argument.required.unwrap_or(false) {
                            format!("{}*", argument.name)
                        } else {
                            argument.name.clone()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }),
            description: prompt.description.clone(),
        })
        .collect::<Vec<_>>();

    let mut exposed_names = state
        .read()
        .expect("MCP state lock poisoned")
        .tools
        .iter()
        .filter(|tool| tool.server != name)
        .map(|tool| tool.exposed_name.clone())
        .collect::<std::collections::BTreeSet<_>>();
    if let Some(collision) = definitions
        .iter()
        .map(|tool| tool.exposed_name.clone())
        .find(|tool| !exposed_names.insert(tool.clone()))
    {
        set_failed(
            state,
            name,
            anyhow!("MCP tool name collision after sanitizing: '{collision}'"),
        );
        return;
    }

    let mut state = state.write().expect("MCP state lock poisoned");
    state.tools.retain(|tool| tool.server != name);
    state.tools.extend(definitions);
    state
        .tools
        .sort_by(|left, right| left.exposed_name.cmp(&right.exposed_name));
    state.servers.insert(
        name.to_owned(),
        McpServerStatus {
            name: name.to_owned(),
            state: McpConnectionState::Ready,
            tools: tool_statuses,
            resources: resource_statuses,
            resource_templates: template_statuses,
            prompts: prompt_statuses,
            error: None,
        },
    );
}

fn server_oauth(config: &McpServerConfig) -> Option<(&str, &super::McpOAuthConfig)> {
    match &config.transport {
        McpTransportConfig::StreamableHttp {
            url,
            oauth: Some(oauth),
            ..
        } => Some((url, oauth)),
        _ => None,
    }
}

async fn new_oauth_state(name: &str, url: &str, cwd: &Path) -> Result<OAuthState> {
    let mut state = OAuthState::new(url, None)
        .await
        .with_context(|| format!("failed to initialize OAuth for MCP server '{name}'"))?;
    let OAuthState::Unauthorized(manager) = &mut state else {
        bail!("OAuth for MCP server '{name}' entered an invalid initial state");
    };
    manager.set_credential_store(FileCredentialStore::new(oauth_credential_path(
        cwd, name, url,
    )));
    Ok(state)
}

async fn restore_oauth_client(
    name: &str,
    config: &McpServerConfig,
    cwd: &Path,
) -> Result<Option<McpOAuthClient>> {
    let Some((url, _)) = server_oauth(config) else {
        return Ok(None);
    };
    let mut state = new_oauth_state(name, url, cwd).await?;
    let restored = match &mut state {
        OAuthState::Unauthorized(manager) => manager.initialize_from_store().await?,
        _ => false,
    };
    if !restored {
        return Ok(None);
    }
    let manager = match state {
        OAuthState::Unauthorized(manager) => manager,
        _ => bail!("stored OAuth credentials for MCP server '{name}' are invalid"),
    };
    Ok(Some(AuthClient::new(reqwest::Client::new(), manager)))
}

async fn begin_oauth_authorization(
    name: &str,
    config: &McpConfig,
    cwd: &Path,
    oauth_states: &mut BTreeMap<String, OAuthState>,
) -> Result<String> {
    let server = config
        .servers
        .get(name)
        .with_context(|| format!("unknown MCP server '{name}'"))?;
    let Some((url, oauth)) = server_oauth(server) else {
        bail!("MCP server '{name}' is not configured for OAuth");
    };
    if matches!(
        &server.transport,
        McpTransportConfig::StreamableHttp {
            bearer_token_env: Some(_),
            ..
        }
    ) {
        bail!("MCP server '{name}' cannot configure both bearer_token_env and oauth");
    }

    let mut oauth_state = new_oauth_state(name, url, cwd).await?;
    let scopes = oauth.scopes.iter().map(String::as_str).collect::<Vec<_>>();
    oauth_state
        .start_authorization(&scopes, &oauth.redirect_uri, Some("Glint"))
        .await
        .with_context(|| format!("failed to start OAuth for MCP server '{name}'"))?;
    let authorization_url = oauth_state
        .get_authorization_url()
        .await
        .with_context(|| format!("failed to create OAuth URL for MCP server '{name}'"))?;
    oauth_states.insert(name.to_owned(), oauth_state);
    Ok(authorization_url)
}

#[allow(clippy::too_many_arguments)]
async fn complete_oauth_authorization(
    name: &str,
    callback_url: &str,
    config: &McpConfig,
    cwd: &Path,
    notifications: tokio_mpsc::UnboundedSender<NotificationEvent>,
    elicitations: mpsc::Sender<McpElicitation>,
    oauth_states: &mut BTreeMap<String, OAuthState>,
    oauth_clients: &mut BTreeMap<String, McpOAuthClient>,
    services: &mut BTreeMap<String, McpService>,
    state: &Arc<RwLock<ManagerState>>,
) -> Result<()> {
    let server = config
        .servers
        .get(name)
        .with_context(|| format!("unknown MCP server '{name}'"))?;
    if server_oauth(server).is_none() {
        bail!("MCP server '{name}' is not configured for OAuth");
    }
    let oauth_state = oauth_states
        .get_mut(name)
        .with_context(|| format!("OAuth was not started for MCP server '{name}'"))?;
    oauth_state
        .handle_callback_url(callback_url)
        .await
        .with_context(|| format!("OAuth callback failed for MCP server '{name}'"))?;
    let oauth_state = oauth_states
        .remove(name)
        .context("completed OAuth state disappeared")?;
    let manager = oauth_state
        .into_authorization_manager()
        .context("OAuth callback did not produce authorized credentials")?;
    let client = AuthClient::new(reqwest::Client::new(), manager);
    oauth_clients.insert(name.to_owned(), client.clone());
    reconnect_server(
        name,
        config,
        cwd,
        notifications,
        elicitations,
        Some(client),
        services,
        state,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn logout_oauth(
    name: &str,
    config: &McpConfig,
    cwd: &Path,
    oauth_states: &mut BTreeMap<String, OAuthState>,
    oauth_clients: &mut BTreeMap<String, McpOAuthClient>,
    services: &mut BTreeMap<String, McpService>,
    state: &Arc<RwLock<ManagerState>>,
) -> Result<()> {
    let server = config
        .servers
        .get(name)
        .with_context(|| format!("unknown MCP server '{name}'"))?;
    let Some((url, _)) = server_oauth(server) else {
        bail!("MCP server '{name}' is not configured for OAuth");
    };
    oauth_states.remove(name);
    oauth_clients.remove(name);
    if let Some(mut service) = services.remove(name) {
        let _ = service.close_with_timeout(Duration::from_secs(2)).await;
    }
    FileCredentialStore::new(oauth_credential_path(cwd, name, url))
        .clear()
        .await?;
    set_failed(
        state,
        name,
        anyhow!("OAuth authorization required; run `/mcp auth {name}`"),
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn reconnect_server(
    name: &str,
    config: &McpConfig,
    cwd: &Path,
    notifications: tokio_mpsc::UnboundedSender<NotificationEvent>,
    elicitations: mpsc::Sender<McpElicitation>,
    oauth_client: Option<McpOAuthClient>,
    services: &mut BTreeMap<String, McpService>,
    state: &Arc<RwLock<ManagerState>>,
) -> Result<()> {
    let server_config = config
        .servers
        .get(name)
        .with_context(|| format!("unknown MCP server '{name}'"))?;
    if let Some(mut old) = services.remove(name) {
        let _ = old.close_with_timeout(Duration::from_secs(2)).await;
    }
    match connect_server(
        name,
        server_config,
        cwd,
        notifications,
        elicitations,
        oauth_client,
    )
    .await
    {
        Ok(service) => {
            services.insert(name.to_owned(), service);
            refresh_server(name, server_config, services, state).await;
            Ok(())
        }
        Err(error) => {
            let message = format!("{error:#}");
            set_failed(state, name, anyhow!(message.clone()));
            bail!(message)
        }
    }
}

async fn execute_operation(
    service: Peer<RoleClient>,
    server: &str,
    operation: McpOperation,
    arguments: Value,
    timeout: Duration,
) -> Result<McpResponse> {
    let operation = async {
        match operation {
            McpOperation::CallTool { name } => {
                let params =
                    CallToolRequestParams::new(name).with_arguments(object_arguments(arguments)?);
                let result = service.call_tool(params).await?;
                Ok(format_call_tool_result(result))
            }
            McpOperation::ListResources => Ok(json_response(service.list_all_resources().await?)),
            McpOperation::ListResourceTemplates => {
                Ok(json_response(service.list_all_resource_templates().await?))
            }
            McpOperation::ReadResource => {
                let uri = required_string(&arguments, "uri")?;
                Ok(json_response(
                    service
                        .read_resource(ReadResourceRequestParams::new(uri))
                        .await?,
                ))
            }
            McpOperation::SubscribeResource => {
                let uri = required_string(&arguments, "uri")?;
                service.subscribe(SubscribeRequestParams::new(uri)).await?;
                Ok(json_response(json!({"subscribed": uri})))
            }
            McpOperation::UnsubscribeResource => {
                let uri = required_string(&arguments, "uri")?;
                service
                    .unsubscribe(UnsubscribeRequestParams::new(uri))
                    .await?;
                Ok(json_response(json!({"unsubscribed": uri})))
            }
            McpOperation::ListPrompts => Ok(json_response(service.list_all_prompts().await?)),
            McpOperation::GetPrompt => {
                let name = required_string(&arguments, "name")?;
                let mut params = GetPromptRequestParams::new(name);
                if let Some(value) = arguments.get("arguments") {
                    params = params.with_arguments(object_arguments(value.clone())?);
                }
                Ok(json_response(service.get_prompt(params).await?))
            }
        }
    };
    time::timeout(timeout, operation)
        .await
        .with_context(|| format!("MCP operation on '{server}' timed out"))?
}

fn format_call_tool_result(result: CallToolResult) -> McpResponse {
    let mut parts = result
        .content
        .iter()
        .map(|content| {
            content
                .raw
                .as_text()
                .map(|text| text.text.clone())
                .unwrap_or_else(|| serde_json::to_string_pretty(content).unwrap_or_default())
        })
        .filter(|content| !content.is_empty())
        .collect::<Vec<_>>();
    if let Some(structured) = result.structured_content {
        parts.push(
            serde_json::to_string_pretty(&structured).unwrap_or_else(|_| structured.to_string()),
        );
    }
    McpResponse {
        content: if parts.is_empty() {
            "MCP tool completed without content.".to_owned()
        } else {
            parts.join("\n")
        },
        is_error: result.is_error.unwrap_or(false),
    }
}

fn json_response(value: impl serde::Serialize) -> McpResponse {
    McpResponse {
        content: serde_json::to_string_pretty(&value)
            .unwrap_or_else(|error| format!("failed to serialize MCP response: {error}")),
        is_error: false,
    }
}

fn resource_tools(server: &str, config: &McpServerConfig) -> Vec<McpToolDefinition> {
    vec![
        gateway_tool(
            server,
            "list_resources",
            "List resources exposed by this MCP server.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
            McpOperation::ListResources,
            config,
        ),
        gateway_tool(
            server,
            "list_resource_templates",
            "List resource URI templates exposed by this MCP server.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
            McpOperation::ListResourceTemplates,
            config,
        ),
        gateway_tool(
            server,
            "read_resource",
            "Read a resource from this MCP server by URI.",
            json!({
                "type":"object",
                "properties":{"uri":{"type":"string"}},
                "required":["uri"],
                "additionalProperties":false
            }),
            McpOperation::ReadResource,
            config,
        ),
        gateway_tool(
            server,
            "subscribe_resource",
            "Subscribe to update notifications for an MCP resource URI.",
            json!({
                "type":"object",
                "properties":{"uri":{"type":"string"}},
                "required":["uri"],
                "additionalProperties":false
            }),
            McpOperation::SubscribeResource,
            config,
        ),
        gateway_tool(
            server,
            "unsubscribe_resource",
            "Stop update notifications for an MCP resource URI.",
            json!({
                "type":"object",
                "properties":{"uri":{"type":"string"}},
                "required":["uri"],
                "additionalProperties":false
            }),
            McpOperation::UnsubscribeResource,
            config,
        ),
    ]
}

fn prompt_tools(server: &str, config: &McpServerConfig) -> Vec<McpToolDefinition> {
    vec![
        gateway_tool(
            server,
            "list_prompts",
            "List prompts exposed by this MCP server.",
            json!({"type":"object","properties":{},"additionalProperties":false}),
            McpOperation::ListPrompts,
            config,
        ),
        gateway_tool(
            server,
            "get_prompt",
            "Render a prompt exposed by this MCP server.",
            json!({
                "type":"object",
                "properties":{
                    "name":{"type":"string"},
                    "arguments":{"type":"object"}
                },
                "required":["name"],
                "additionalProperties":false
            }),
            McpOperation::GetPrompt,
            config,
        ),
    ]
}

fn gateway_tool(
    server: &str,
    suffix: &str,
    description: &str,
    parameters: Value,
    operation: McpOperation,
    config: &McpServerConfig,
) -> McpToolDefinition {
    McpToolDefinition {
        exposed_name: exposed_tool_name(server, suffix),
        description: description.to_owned(),
        parameters,
        server: server.to_owned(),
        operation,
        approval: config.approval_for_tool(suffix),
        concurrency_safe: true,
        timeout: Duration::from_millis(config.tool_timeout_ms),
    }
}

fn object_arguments(value: Value) -> Result<Map<String, Value>> {
    value
        .as_object()
        .cloned()
        .context("MCP arguments must be a JSON object")
}

fn required_string<'a>(arguments: &'a Value, key: &str) -> Result<&'a str> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("missing string argument '{key}'"))
}

fn resolve_server_cwd(configured: Option<&str>, fallback: &Path) -> PathBuf {
    configured
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                fallback.join(path)
            }
        })
        .unwrap_or_else(|| fallback.to_path_buf())
}

fn exposed_tool_name(server: &str, tool: &str) -> String {
    format!(
        "mcp__{}__{}",
        sanitize_tool_name(server),
        sanitize_tool_name(tool)
    )
}

fn oauth_credential_path(cwd: &Path, server: &str, url: &str) -> PathBuf {
    let root = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| cwd.to_path_buf())
        .join(".glint/mcp/oauth");
    let filename = format!(
        "{}-{:016x}.json",
        sanitize_tool_name(server),
        stable_hash(&format!("{server}\0{url}"))
    );
    root.join(filename)
}

fn stable_hash(value: &str) -> u64 {
    value
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        })
}

fn auth_storage_error(error: impl std::fmt::Display) -> AuthError {
    AuthError::InternalError(format!("OAuth credential storage failed: {error}"))
}

fn write_private_file(path: &Path, contents: &[u8]) -> std::result::Result<(), AuthError> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(auth_storage_error)?;
    file.write_all(contents).map_err(auth_storage_error)?;
    file.sync_all().map_err(auth_storage_error)
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> std::result::Result<(), AuthError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(auth_storage_error)
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> std::result::Result<(), AuthError> {
    Ok(())
}

fn failed_status(name: &str, error: String) -> McpServerStatus {
    McpServerStatus {
        name: name.to_owned(),
        state: McpConnectionState::Failed,
        tools: Vec::new(),
        resources: Vec::new(),
        resource_templates: Vec::new(),
        prompts: Vec::new(),
        error: Some(error),
    }
}

fn set_failed(state: &Arc<RwLock<ManagerState>>, name: &str, error: anyhow::Error) {
    let mut state = state.write().expect("MCP state lock poisoned");
    state.tools.retain(|tool| tool.server != name);
    state
        .servers
        .insert(name.to_owned(), failed_status(name, format!("{error:#}")));
}

fn tool_error(call: &ToolCall, content: String) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        content,
        is_error: true,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::tools::ToolRegistry;

    #[test]
    fn oauth_credentials_are_persisted_and_cleared() {
        let root = std::env::temp_dir().join(format!("glint-oauth-store-{}", uuid::Uuid::new_v4()));
        let path = root.join("credentials.json");
        let store = FileCredentialStore::new(path.clone());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            store
                .save(StoredCredentials::new(
                    "client-id".to_owned(),
                    None,
                    vec!["read".to_owned()],
                    None,
                ))
                .await
                .unwrap();
            let restored = store.load().await.unwrap().unwrap();
            assert_eq!(restored.client_id, "client-id");
            assert_eq!(restored.granted_scopes, ["read"]);
            store.clear().await.unwrap();
            assert!(store.load().await.unwrap().is_none());
        });
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn stdio_server_tools_resources_and_prompts_join_the_registry() {
        if std::process::Command::new("python3")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let script = r#"
import json, sys
for line in sys.stdin:
    message = json.loads(line)
    if "id" not in message:
        continue
    method = message.get("method")
    request_id = message["id"]
    if method == "initialize":
        result = {
            "protocolVersion": message["params"]["protocolVersion"],
            "capabilities": {
                "tools": {"listChanged": True},
                "resources": {"subscribe": False, "listChanged": True},
                "prompts": {"listChanged": True}
            },
            "serverInfo": {"name": "glint-test", "version": "1"}
        }
    elif method == "tools/list":
        result = {"tools": [
            {
                "name": "echo",
                "description": "Echo text",
                "inputSchema": {
                    "type": "object",
                    "properties": {"message": {"type": "string"}},
                    "required": ["message"]
                },
                "annotations": {"readOnlyHint": True}
            },
            {
                "name": "ask",
                "description": "Request user input",
                "inputSchema": {"type": "object", "properties": {}}
            }
        ]}
    elif method == "tools/call":
        if message["params"]["name"] == "ask":
            print(json.dumps({
                "jsonrpc": "2.0",
                "id": "elicitation-1",
                "method": "elicitation/create",
                "params": {
                    "mode": "form",
                    "message": "Provide a label",
                    "requestedSchema": {
                        "type": "object",
                        "properties": {"label": {"type": "string"}},
                        "required": ["label"]
                    }
                }
            }), flush=True)
            elicitation = json.loads(sys.stdin.readline())
            text = elicitation["result"]["action"]
        else:
            text = message["params"]["arguments"]["message"]
        result = {"content": [{"type": "text", "text": text}]}
    elif method == "resources/list":
        result = {"resources": [{"uri": "test://hello", "name": "hello"}]}
    elif method == "resources/templates/list":
        result = {"resourceTemplates": []}
    elif method == "resources/read":
        result = {"contents": [{"uri": message["params"]["uri"], "text": "resource text"}]}
    elif method == "prompts/list":
        result = {"prompts": [{"name": "hello", "description": "Hello prompt"}]}
    elif method == "prompts/get":
        result = {"description": "Hello", "messages": [{"role": "user", "content": {"type": "text", "text": "hello"}}]}
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": request_id, "error": {"code": -32601, "message": "not found"}}), flush=True)
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)
"#;
        let config = McpConfig {
            servers: BTreeMap::from([(
                "demo".to_owned(),
                McpServerConfig {
                    enabled: true,
                    startup_timeout_ms: 5_000,
                    tool_timeout_ms: 5_000,
                    approval: McpApprovalPolicy::Allow,
                    tool_approval: BTreeMap::new(),
                    enabled_tools: None,
                    disabled_tools: Vec::new(),
                    transport: McpTransportConfig::Stdio {
                        command: "python3".to_owned(),
                        args: vec!["-u".to_owned(), "-c".to_owned(), script.to_owned()],
                        env: BTreeMap::new(),
                        env_vars: Vec::new(),
                        cwd: None,
                    },
                },
            )]),
        };
        let manager = McpManager::new(config, std::env::temp_dir());
        let registry = ToolRegistry::new().with_dynamic_tools(manager.dynamic_tools());
        let names = registry
            .specs()
            .into_iter()
            .map(|spec| spec.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"mcp__demo__echo".to_owned()));
        assert!(names.contains(&"mcp__demo__read_resource".to_owned()));
        assert!(names.contains(&"mcp__demo__get_prompt".to_owned()));

        let status = manager.statuses().into_iter().next().unwrap();
        assert_eq!(status.state, McpConnectionState::Ready);
        assert!(status.tools.iter().any(|tool| {
            tool.name == "mcp__demo__echo"
                && tool.description == "Echo text"
                && tool.read_only
                && tool.approval == McpApprovalPolicy::Allow
        }));
        assert!(
            status
                .tools
                .iter()
                .any(|tool| { tool.name == "mcp__demo__read_resource" && tool.read_only })
        );
        assert_eq!(
            status.resources,
            [McpCapabilityStatus {
                name: "hello".to_owned(),
                detail: Some("test://hello".to_owned()),
                description: None,
            }]
        );
        assert_eq!(
            status.prompts,
            [McpCapabilityStatus {
                name: "hello".to_owned(),
                detail: None,
                description: Some("Hello prompt".to_owned()),
            }]
        );

        let result = registry.execute(&ToolCall {
            id: "echo".to_owned(),
            name: "mcp__demo__echo".to_owned(),
            arguments: json!({"message":"hello from MCP"}),
        });
        assert!(!result.is_error, "{}", result.content);
        assert_eq!(result.content, "hello from MCP");

        let elicitation_registry = registry.clone();
        let elicitation_call = std::thread::spawn(move || {
            elicitation_registry.execute(&ToolCall {
                id: "ask".to_owned(),
                name: "mcp__demo__ask".to_owned(),
                arguments: json!({}),
            })
        });
        let elicitation = (0..100)
            .find_map(|_| {
                let request = manager.try_recv_elicitation();
                if request.is_none() {
                    std::thread::sleep(Duration::from_millis(10));
                }
                request
            })
            .expect("MCP server should request elicitation");
        assert!(matches!(
            elicitation.request,
            McpElicitationRequest::Form { .. }
        ));
        elicitation.respond(true, Some(json!({"label":"accepted"})));
        let elicitation_result = elicitation_call.join().unwrap();
        assert_eq!(elicitation_result.content, "accept");
        assert!(manager.status_text().contains("demo  ready"));
        manager.shutdown();
    }
}
