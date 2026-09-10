//! Native subscription OAuth. API keys and Codex-owned files are never modified.
use super::{ReasoningLevel, ReasoningOptions};
use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
const ISSUER: &str = "https://auth.openai.com";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const REDIRECT: &str = "http://localhost:1455/auth/callback";
const AUTH_HELP: &str =
    "ChatGPT authentication required. Open /model → Add model → ChatGPT to sign in again.";
static AUTH_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodexModel {
    pub id: String,
    pub display_name: String,
    pub is_default: bool,
}
#[derive(Clone, Debug)]
pub enum LoginEvent {
    Url(String),
    Completed(Vec<CodexModel>),
    Failed(String),
}
// Deliberately no Debug implementation: access tokens must not enter logs.
pub struct Credentials {
    pub access_token: String,
    pub account_id: String,
    pub compute_residency: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct StoredTokens {
    access_token: String,
    refresh_token: String,
    account_id: String,
    expires_at: u64,
}
struct LoginState {
    cancelled: Arc<AtomicBool>,
    events: Mutex<Receiver<LoginEvent>>,
}
impl Drop for LoginState {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Release);
    }
}
#[derive(Clone)]
pub struct LoginHandle(Arc<LoginState>);
impl std::fmt::Debug for LoginHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginHandle").finish_non_exhaustive()
    }
}
impl LoginHandle {
    pub fn start(root: PathBuf) -> Self {
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        thread::spawn(move || {
            if let Err(error) = login(&root, &sender, &flag)
                && !flag.load(Ordering::Acquire)
            {
                let _ = sender.send(LoginEvent::Failed(error.to_string()));
            }
        });
        Self(Arc::new(LoginState {
            cancelled,
            events: Mutex::new(receiver),
        }))
    }
    pub fn poll(&self) -> Vec<LoginEvent> {
        self.0
            .events
            .lock()
            .map(|events| events.try_iter().collect())
            .unwrap_or_default()
    }
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn client() -> Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("glint/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("Could not create ChatGPT client")
}
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}
fn random_secret() -> String {
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}
fn claims(token: &str) -> Option<Value> {
    let bytes = URL_SAFE_NO_PAD.decode(token.split('.').nth(1)?).ok()?;
    serde_json::from_slice(&bytes).ok()
}
fn residency(token: &str) -> Option<String> {
    let value = claims(token)?;
    value["https://api.openai.com/auth"]["chatgpt_compute_residency"]
        .as_str()
        .or_else(|| value["chatgpt_compute_residency"].as_str())
        .filter(|s| !s.is_empty() && *s != "no_constraint")
        .map(str::to_owned)
}
fn account(value: &Value) -> Option<String> {
    value["chatgpt_account_id"]
        .as_str()
        .or_else(|| value["https://api.openai.com/auth"]["chatgpt_account_id"].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
fn token_string(value: &Value, field: &str) -> Option<String> {
    value[field]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}
fn tokens_from_response(value: &Value, previous: Option<&StoredTokens>) -> Result<StoredTokens> {
    let access_token = token_string(value, "access_token")
        .context("ChatGPT token response missing access token")?;
    let access_claims = claims(&access_token);
    let account_id = token_string(value, "id_token")
        .and_then(|s| claims(&s))
        .as_ref()
        .and_then(account)
        .or_else(|| access_claims.as_ref().and_then(account))
        .or_else(|| previous.map(|p| p.account_id.clone()))
        .context("ChatGPT token response missing account identity")?;
    let refresh_token = token_string(value, "refresh_token")
        .or_else(|| previous.map(|p| p.refresh_token.clone()))
        .context("ChatGPT token response missing refresh token")?;
    let expires_at = value["expires_in"]
        .as_u64()
        .map(|ttl| now().saturating_add(ttl))
        .or_else(|| access_claims.as_ref().and_then(|c| c["exp"].as_u64()))
        .unwrap_or(now());
    Ok(StoredTokens {
        access_token,
        refresh_token,
        account_id,
        expires_at,
    })
}
fn atomic_write(root: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    fs::create_dir_all(root).context("Could not create ChatGPT state directory")?;
    let temporary = root.join(format!(".{name}.{}", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options
            .open(&temporary)
            .context("Could not create protected ChatGPT state file")?;
        file.write_all(bytes)
            .context("Could not write ChatGPT state")?;
        file.sync_all().context("Could not sync ChatGPT state")?;
        fs::rename(&temporary, root.join(name)).context("Could not save ChatGPT state")?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}
fn save_tokens(root: &Path, tokens: &StoredTokens) -> Result<()> {
    atomic_write(root, "chatgpt-auth.json", &serde_json::to_vec(tokens)?)
}
fn load_tokens(root: &Path) -> Result<Option<StoredTokens>> {
    match fs::read(root.join("chatgpt-auth.json")) {
        Ok(bytes) => {
            let tokens: StoredTokens = serde_json::from_slice(&bytes)
                .map_err(|_| anyhow::anyhow!("Invalid ChatGPT authentication file. {AUTH_HELP}"))?;
            if tokens.access_token.is_empty()
                || tokens.refresh_token.is_empty()
                || tokens.account_id.is_empty()
            {
                bail!("{AUTH_HELP}");
            }
            return Ok(Some(tokens));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => bail!("Could not read ChatGPT authentication file"),
    }
    let bytes = match fs::read(root.join("codex/auth.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => bail!("Could not read previous ChatGPT authentication"),
    };
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("Invalid previous ChatGPT authentication"))?;
    let value = &value["tokens"];
    let Some(access_token) = token_string(value, "access_token") else {
        return Ok(None);
    };
    let Some(refresh_token) = token_string(value, "refresh_token") else {
        return Ok(None);
    };
    let account_id = token_string(value, "account_id")
        .or_else(|| claims(&access_token).as_ref().and_then(account))
        .or_else(|| {
            token_string(value, "id_token")
                .and_then(|s| claims(&s))
                .as_ref()
                .and_then(account)
        })
        .context(AUTH_HELP)?;
    let expires_at = claims(&access_token)
        .and_then(|c| c["exp"].as_u64())
        .unwrap_or(0);
    let tokens = StoredTokens {
        access_token,
        refresh_token,
        account_id,
        expires_at,
    };
    save_tokens(root, &tokens)?;
    Ok(Some(tokens))
}
fn auth_file_lock(root: &Path) -> Result<std::fs::File> {
    fs::create_dir_all(root).context("Could not create ChatGPT state directory")?;
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(root.join(".chatgpt-auth.lock"))
        .context("Could not open ChatGPT authentication lock")?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50))
            }
            Err(_) => bail!("ChatGPT authentication is busy; retry shortly"),
        }
    }
}
fn token_request(
    http: &reqwest::blocking::Client,
    endpoint: &str,
    form: &[(&str, &str)],
) -> Result<Value> {
    let response = http
        .post(format!("{endpoint}/oauth/token"))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(
            reqwest::Url::parse_with_params("http://localhost", form)?
                .query()
                .unwrap_or("")
                .to_owned(),
        )
        .send()
        .map_err(|_| anyhow::anyhow!("ChatGPT token request failed; check your connection"))?;
    if !response.status().is_success() {
        bail!(
            "ChatGPT token request failed (HTTP {}). {AUTH_HELP}",
            response.status().as_u16()
        );
    }
    response
        .json()
        .map_err(|_| anyhow::anyhow!("ChatGPT returned an invalid token response"))
}
pub fn credentials() -> Result<Credentials> {
    credentials_at(crate::paths::GlintPaths::discover()?.root(), ISSUER)
}
fn credentials_at(root: &Path, issuer: &str) -> Result<Credentials> {
    credentials_with_client(root, issuer, &client()?)
}
fn credentials_with_client(
    root: &Path,
    issuer: &str,
    http: &reqwest::blocking::Client,
) -> Result<Credentials> {
    let _lock = AUTH_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("ChatGPT authentication lock unavailable"))?;
    let _file_lock = auth_file_lock(root)?;
    let mut tokens = load_tokens(root)?.context(AUTH_HELP)?;
    if tokens.expires_at <= now().saturating_add(60) {
        let response = token_request(
            http,
            issuer,
            &[
                ("grant_type", "refresh_token"),
                ("refresh_token", &tokens.refresh_token),
                ("client_id", CLIENT_ID),
            ],
        )?;
        tokens = tokens_from_response(&response, Some(&tokens))?;
        save_tokens(root, &tokens)?;
    }
    Ok(Credentials {
        compute_residency: residency(&tokens.access_token),
        access_token: tokens.access_token,
        account_id: tokens.account_id,
    })
}
fn callback_code(target: &str, expected_state: &str) -> Result<String> {
    let url = reqwest::Url::parse(&format!("http://localhost{target}"))
        .context("Invalid ChatGPT callback")?;
    if url.path() != "/auth/callback" {
        bail!("Unexpected ChatGPT callback path");
    }
    let pairs: Vec<_> = url.query_pairs().collect();
    let states: Vec<_> = pairs.iter().filter(|(key, _)| key == "state").collect();
    if states.len() != 1 || states[0].1 != expected_state {
        bail!("Invalid ChatGPT login state");
    }
    if pairs.iter().any(|(key, _)| key == "error") {
        bail!("ChatGPT authorization was declined");
    }
    let codes: Vec<_> = pairs.iter().filter(|(key, _)| key == "code").collect();
    if codes.len() != 1 || codes[0].1.is_empty() {
        bail!("Missing or ambiguous ChatGPT authorization code");
    }
    Ok(codes[0].1.to_string())
}
fn check_cancel(cancelled: &AtomicBool) -> Result<()> {
    if cancelled.load(Ordering::Acquire) {
        bail!("ChatGPT sign-in cancelled");
    }
    Ok(())
}
fn wait_callback(listener: TcpListener, state: &str, cancelled: &AtomicBool) -> Result<String> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        check_cancel(cancelled)?;
        if Instant::now() >= deadline {
            bail!("ChatGPT sign-in timed out. Try Add model again.");
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_read_timeout(Some(Duration::from_millis(250)))?;
                stream.set_write_timeout(Some(Duration::from_millis(250)))?;
                let mut bytes = Vec::new();
                let mut buffer = [0; 1024];
                let read_deadline = Instant::now() + Duration::from_secs(2);
                while !bytes.windows(4).any(|w| w == b"\r\n\r\n")
                    && bytes.len() < 8192
                    && Instant::now() < read_deadline
                {
                    check_cancel(cancelled)?;
                    match stream.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => bytes.extend_from_slice(&buffer[..n]),
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(_) => break,
                    }
                }
                let text = String::from_utf8_lossy(&bytes);
                let mut line = text.lines().next().unwrap_or("").split_whitespace();
                let method = line.next().unwrap_or("");
                let target = line.next().unwrap_or("");
                let result = if method == "GET" {
                    callback_code(target, state)
                } else {
                    Err(anyhow::anyhow!("Invalid ChatGPT callback method"))
                };
                let (status, body) = if result.is_ok() {
                    (
                        "200 OK",
                        "Authorization received. Return to Glint to finish signing in.",
                    )
                } else {
                    (
                        "400 Bad Request",
                        "Invalid authorization callback. Return to Glint and try again.",
                    )
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                // Unrelated requests and wrong-state callbacks cannot terminate a valid pending login.
                if result.is_ok()
                    || result.as_ref().err().is_some_and(|error| {
                        error.to_string() == "ChatGPT authorization was declined"
                    })
                {
                    return result;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50))
            }
            Err(_) => bail!("ChatGPT callback listener failed"),
        }
    }
}
fn fetch_models(credentials: &Credentials) -> Result<(Vec<CodexModel>, Value)> {
    let response = client()?
        .get("https://chatgpt.com/backend-api/codex/models?client_version=0.153.4")
        .bearer_auth(&credentials.access_token)
        .header("ChatGPT-Account-Id", &credentials.account_id)
        .header("originator", "glint")
        .send()
        .map_err(|_| anyhow::anyhow!("Could not load ChatGPT models; check your connection"))?;
    if !response.status().is_success() {
        bail!(
            "Could not load ChatGPT models (HTTP {}). {AUTH_HELP}",
            response.status().as_u16()
        );
    }
    let value: Value = response
        .json()
        .map_err(|_| anyhow::anyhow!("Invalid ChatGPT model catalog"))?;
    let models = parse_models(&value)?;
    Ok((models, value))
}
fn parse_models(value: &Value) -> Result<Vec<CodexModel>> {
    let mut entries: Vec<_> = value["models"]
        .as_array()
        .context("Invalid ChatGPT model catalog")?
        .iter()
        .filter(|m| m["visibility"].as_str() == Some("list"))
        .collect();
    entries.sort_by_key(|m| m["priority"].as_i64().unwrap_or(i64::MAX));
    let mut seen = HashSet::new();
    let mut models = Vec::new();
    for model in entries {
        let id = model["slug"]
            .as_str()
            .filter(|s| !s.is_empty())
            .context("ChatGPT model is missing an identifier")?;
        if seen.insert(id.to_owned()) {
            models.push(CodexModel {
                id: id.to_owned(),
                display_name: model["display_name"].as_str().unwrap_or(id).to_owned(),
                is_default: models.is_empty(),
            });
        }
    }
    if models.is_empty() {
        bail!("No models are available for this ChatGPT account");
    }
    Ok(models)
}
pub fn cached_reasoning_options(root: &Path, model: &str) -> ReasoningOptions {
    let Some(value) = cached_catalog(root) else {
        return ReasoningOptions::default();
    };
    let Some(entry) = value["models"]
        .as_array()
        .and_then(|models| models.iter().find(|entry| entry["slug"] == model))
    else {
        return ReasoningOptions::default();
    };
    let mut seen = HashSet::new();
    let levels: Vec<_> = entry["supported_reasoning_levels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|level| {
            let effort = level["effort"].as_str().filter(|s| !s.trim().is_empty())?;
            if !seen.insert(effort.to_owned()) {
                return None;
            }
            Some(ReasoningLevel {
                effort: effort.to_owned(),
                description: level["description"].as_str().unwrap_or("").to_owned(),
            })
        })
        .collect();
    let default_effort = entry["default_reasoning_level"]
        .as_str()
        .filter(|effort| levels.iter().any(|level| level.effort == *effort))
        .map(str::to_owned);
    ReasoningOptions {
        default_effort,
        levels,
    }
}
fn cached_catalog(root: &Path) -> Option<Value> {
    let bytes = match fs::read(root.join("chatgpt-models.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::read(root.join("codex/models_cache.json")).ok()?
        }
        Err(_) => return None,
    };
    serde_json::from_slice(&bytes).ok()
}
pub fn cached_context_window(model: &str) -> Option<u64> {
    let root = crate::paths::GlintPaths::discover().ok()?;
    cached_context_window_at(root.root(), model)
}
fn cached_context_window_at(root: &Path, model: &str) -> Option<u64> {
    let value = cached_catalog(root)?;
    value["models"]
        .as_array()?
        .iter()
        .find(|entry| entry["slug"] == model)?["context_window"]
        .as_u64()
        .filter(|n| *n > 0)
}
fn login(root: &Path, events: &Sender<LoginEvent>, cancelled: &AtomicBool) -> Result<()> {
    check_cancel(cancelled)?;
    // Try existing tokens first, but an expired/revoked token must still permit signing in again.
    if let Ok(credentials) = credentials_at(root, ISSUER) {
        check_cancel(cancelled)?;
        if let Ok((models, catalog)) = fetch_models(&credentials) {
            check_cancel(cancelled)?;
            atomic_write(root, "chatgpt-models.json", &serde_json::to_vec(&catalog)?)?;
            let _ = events.send(LoginEvent::Completed(models));
            return Ok(());
        }
    }
    check_cancel(cancelled)?;
    let listener = TcpListener::bind("127.0.0.1:1455").context(
        "Could not listen on localhost:1455. Close any other ChatGPT sign-in window and retry",
    )?;
    let verifier = random_secret();
    let state = random_secret();
    let challenge = challenge(&verifier);
    let mut url = reqwest::Url::parse(&format!("{ISSUER}/oauth/authorize"))?;
    url.query_pairs_mut().extend_pairs([
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", REDIRECT),
        ("scope", "openid profile email offline_access"),
        ("code_challenge", &challenge),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("state", &state),
        ("originator", "glint"),
    ]);
    let _ = events.send(LoginEvent::Url(url.to_string()));
    let code = wait_callback(listener, &state, cancelled)?;
    check_cancel(cancelled)?;
    let response = token_request(
        &client()?,
        ISSUER,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", REDIRECT),
            ("client_id", CLIENT_ID),
            ("code_verifier", &verifier),
        ],
    )?;
    let tokens = tokens_from_response(&response, None)?;
    check_cancel(cancelled)?;
    let credentials = Credentials {
        compute_residency: residency(&tokens.access_token),
        access_token: tokens.access_token.clone(),
        account_id: tokens.account_id.clone(),
    };
    let (models, catalog) = fetch_models(&credentials)?;
    check_cancel(cancelled)?;
    let _lock = AUTH_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("ChatGPT authentication lock unavailable"))?;
    let _file_lock = auth_file_lock(root)?;
    check_cancel(cancelled)?;
    save_tokens(root, &tokens)?;
    atomic_write(root, "chatgpt-models.json", &serde_json::to_vec(&catalog)?)?;
    let _ = events.send(LoginEvent::Completed(models));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migrated_model_context_uses_legacy_cache_until_native_discovery() {
        let root = root();
        fs::create_dir(root.join("codex")).unwrap();
        fs::write(
            root.join("codex/models_cache.json"),
            r#"{"models":[{"slug":"demo","context_window":200000}]}"#,
        )
        .unwrap();
        assert_eq!(cached_context_window_at(&root, "demo"), Some(200000));
        fs::write(
            root.join("chatgpt-models.json"),
            r#"{"models":[{"slug":"demo","context_window":300000}]}"#,
        )
        .unwrap();
        assert_eq!(cached_context_window_at(&root, "demo"), Some(300000));
        assert_eq!(cached_context_window_at(&root, "missing"), None);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pkce_matches_rfc7636_vector() {
        assert_eq!(
            challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }
    #[test]
    fn callback_rejects_wrong_state_and_duplicate_code() {
        assert!(callback_code("/auth/callback?state=other&code=secret", "expected").is_err());
        assert!(callback_code("/auth/callback?state=expected&code=a&code=b", "expected").is_err());
        assert_eq!(
            callback_code("/auth/callback?state=expected&code=ok", "expected").unwrap(),
            "ok"
        );
    }
    #[test]
    fn catalog_filters_hidden_and_orders_default_by_priority() {
        let value = serde_json::json!({"models": [
            {"slug":"hidden", "visibility":"hide", "priority":0},
            {"slug":"later", "display_name":"Later", "visibility":"list", "priority":9},
            {"slug":"first", "display_name":"First", "visibility":"list", "priority":1}
        ]});
        let models = parse_models(&value).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "first");
        assert!(models[0].is_default);
        assert!(!models[1].is_default);
        assert!(parse_models(&serde_json::json!({"models":[]})).is_err());
    }
    #[test]
    fn refresh_error_does_not_leak_response_tokens_or_modify_auth() {
        let root = root();
        let mut tokens = sample();
        tokens.expires_at = 0;
        save_tokens(&root, &tokens).unwrap();
        let before = std::fs::read(root.join("chatgpt-auth.json")).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = [0; 4096];
            let _ = stream.read(&mut bytes);
            let body = "secret-access-token";
            write!(
                stream,
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
        });
        let error = credentials_with_client(&root, &endpoint, &test_client())
            .err()
            .unwrap()
            .to_string();
        worker.join().unwrap();
        assert!(error.contains("401"));
        assert!(!error.contains("secret-access-token"));
        assert_eq!(
            std::fs::read(root.join("chatgpt-auth.json")).unwrap(),
            before
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn callback_denial_with_valid_state_terminates_promptly() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let worker =
            thread::spawn(move || wait_callback(listener, "valid", &AtomicBool::new(false)));
        let mut stream = std::net::TcpStream::connect(addr).unwrap();
        write!(
            stream,
            "GET /auth/callback?state=valid&error=access_denied HTTP/1.1\r\nHost: localhost\r\n\r\n"
        )
        .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        assert!(
            worker
                .join()
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("declined")
        );
    }
    #[test]
    fn cancellation_interrupts_active_callback_wait() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let flag = Arc::new(AtomicBool::new(false));
        let worker_flag = flag.clone();
        let worker = thread::spawn(move || wait_callback(listener, "state", &worker_flag));
        thread::sleep(Duration::from_millis(30));
        let start = Instant::now();
        flag.store(true, Ordering::Release);
        assert!(
            worker
                .join()
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
        assert!(start.elapsed() < Duration::from_secs(1));
    }
    #[test]
    fn file_lock_excludes_other_open_handles() {
        let root = root();
        let lock = auth_file_lock(&root).unwrap();
        let another = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join(".chatgpt-auth.lock"))
            .unwrap();
        assert!(matches!(
            another.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        drop(lock);
        another.try_lock().unwrap();
        drop(another);
        std::fs::remove_dir_all(root).unwrap();
    }
    fn test_client() -> reqwest::blocking::Client {
        reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap()
    }
    fn root() -> PathBuf {
        let path = std::env::temp_dir().join(format!("glint-auth-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    fn sample() -> StoredTokens {
        StoredTokens {
            access_token: "test-access".into(),
            refresh_token: "test-refresh".into(),
            account_id: "account".into(),
            expires_at: now() + 3600,
        }
    }
    #[test]
    fn persistence_is_private_and_legacy_import_preserves_original() {
        let root = root();
        std::fs::create_dir(root.join("codex")).unwrap();
        let legacy = serde_json::json!({"tokens":{"access_token":"old-access", "refresh_token":"old-refresh", "account_id":"old-account"}}).to_string();
        std::fs::write(root.join("codex/auth.json"), &legacy).unwrap();
        assert_eq!(
            load_tokens(&root).unwrap().unwrap().account_id,
            "old-account"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("codex/auth.json")).unwrap(),
            legacy
        );
        save_tokens(&root, &sample()).unwrap();
        assert_eq!(load_tokens(&root).unwrap().unwrap().account_id, "account");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(root.join("chatgpt-auth.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn refresh_rotates_and_saves_tokens_without_changing_original() {
        let root = root();
        let mut expired = sample();
        expired.expires_at = 0;
        save_tokens(&root, &expired).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = [0; 4096];
            let _ = stream.read(&mut bytes).unwrap();
            let body = r#"{"access_token":"renewed-access","refresh_token":"rotated-refresh","expires_in":3600}"#;
            write!(stream,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",body.len(),body).unwrap();
        });
        assert_eq!(
            credentials_with_client(&root, &endpoint, &test_client())
                .unwrap()
                .access_token,
            "renewed-access"
        );
        worker.join().unwrap();
        assert_eq!(
            load_tokens(&root).unwrap().unwrap().refresh_token,
            "rotated-refresh"
        );
        assert_eq!(
            credentials_with_client(&root, "http://127.0.0.1:1", &test_client())
                .unwrap()
                .access_token,
            "renewed-access"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn callback_wait_can_be_cancelled_before_any_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        assert!(
            wait_callback(listener, "state", &AtomicBool::new(true))
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
    }
}
