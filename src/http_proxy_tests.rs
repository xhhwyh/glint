use std::{
    fs,
    io::{self, Read, Write},
    net::TcpListener,
    path::PathBuf,
    process::Command,
    thread,
};

use crate::{
    agent::{
        openai::OpenAiProvider,
        provider::{ModelMessage, ModelProvider, ModelRequest},
    },
    config::{LlmConfig, LspConfig, PromptCacheConfig},
    plugins::{PluginManager, PluginsConfig},
    services::mcp::McpConfig,
};

#[test]
fn llm_and_marketplace_http_follow_proxy_environment() {
    let (proxy_url, proxy_thread) = spawn_proxy_server(3);
    let status = proxy_test_command("proxy")
        .env("HTTP_PROXY", &proxy_url)
        .status()
        .unwrap();

    assert!(status.success());
    proxy_thread.join().unwrap();
}

#[test]
fn no_proxy_bypasses_proxy_for_local_llm_endpoint() {
    let (target_url, target_thread) = spawn_chat_server("direct target");
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    proxy.set_nonblocking(true).unwrap();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    let status = proxy_test_command("no-proxy")
        .env("HTTP_PROXY", proxy_url)
        .env("NO_PROXY", "127.0.0.1")
        .env("GLINT_PROXY_TEST_TARGET", target_url)
        .status()
        .unwrap();

    assert!(status.success());
    assert_eq!(
        proxy.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    target_thread.join().unwrap();
}

#[test]
#[ignore = "run as an isolated child by the proxy environment tests"]
fn proxy_environment_child() {
    match std::env::var("GLINT_PROXY_TEST_MODE").unwrap().as_str() {
        "proxy" => {
            assert_streamed_response("http://proxy-only.invalid", "through proxy");

            let root = test_directory("proxy-marketplace");
            let plugins = PluginsConfig {
                cache_dir: Some(root.join("cache")),
                ..Default::default()
            };
            let added = PluginManager::add_marketplace(
                &plugins,
                McpConfig::default(),
                LspConfig::default(),
                &root,
                "http://proxy-only.invalid/marketplace.json",
            )
            .unwrap();
            assert_eq!(added.load.catalog.marketplaces[0].name, "proxy-market");
            fs::remove_dir_all(root).unwrap();
        }
        "no-proxy" => assert_streamed_response(
            &std::env::var("GLINT_PROXY_TEST_TARGET").unwrap(),
            "direct target",
        ),
        mode => panic!("unknown proxy test mode: {mode}"),
    }
}

fn proxy_test_command(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--ignored",
            "--exact",
            "http_proxy_tests::proxy_environment_child",
            "--nocapture",
        ])
        .env_clear()
        .env("GLINT_PROXY_TEST_MODE", mode);
    command
}

fn assert_streamed_response(base_url: &str, expected: &str) {
    let mut provider = OpenAiProvider::new(test_llm_config(base_url));
    let mut deltas = Vec::new();
    let response = provider
        .stream(
            ModelRequest {
                messages: vec![ModelMessage::user("hello")],
                tools: Vec::new(),
                max_tokens: Some(32),
            },
            &mut |delta| deltas.push(delta),
        )
        .unwrap();
    assert_eq!(deltas, [expected]);
    assert_eq!(response.assistant_text.as_deref(), Some(expected));
}

fn test_llm_config(base_url: &str) -> LlmConfig {
    LlmConfig {
        provider: "test".to_owned(),
        base_url: base_url.to_owned(),
        model: "test-model".to_owned(),
        providers: Vec::new(),
        temperature: 0.0,
        max_tokens: 32,
        context_window: None,
        api_key: "test-key".to_owned(),
        default_context_window: None,
        prompt_cache: PromptCacheConfig::default(),
    }
}

fn test_directory(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("glint-{label}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&path).unwrap();
    path
}

fn spawn_proxy_server(requests: usize) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        for _ in 0..requests {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_headers(&mut stream);
            if request.starts_with("POST ") {
                write_streaming_response(&mut stream, "through proxy");
            } else {
                write_json_response(&mut stream, r#"{"name":"proxy-market","plugins":[]}"#);
            }
        }
    });
    (url, handle)
}

fn spawn_chat_server(text: &'static str) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_http_headers(&mut stream);
        write_streaming_response(&mut stream, text);
    });
    (url, handle)
}

fn read_http_headers(stream: &mut impl Read) -> String {
    let mut request = Vec::new();
    let mut byte = [0_u8; 1];
    while !request.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).unwrap();
        request.push(byte[0]);
    }
    let headers = String::from_utf8(request).unwrap();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or_default();
    let mut body = vec![0; content_length];
    stream.read_exact(&mut body).unwrap();
    headers
}

fn write_streaming_response(stream: &mut impl Write, text: &str) {
    let body = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}},\"finish_reason\":\"stop\"}}],\"usage\":null}}\n\ndata: [DONE]\n\n"
    );
    write_http_response(stream, "text/event-stream", &body);
}

fn write_json_response(stream: &mut impl Write, body: &str) {
    write_http_response(stream, "application/json", body);
}

fn write_http_response(stream: &mut impl Write, content_type: &str, body: &str) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    stream.flush().unwrap();
}
