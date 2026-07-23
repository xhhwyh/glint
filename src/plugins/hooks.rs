use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::{HookEvent, PluginHook};

#[derive(Clone, Default)]
pub struct HookRunner {
    hooks: Vec<PluginHook>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct HookOutcome {
    pub replacement: Option<Value>,
}

struct HookOutput {
    decision: HookDecision,
    reason: Option<String>,
    replacement: Option<Value>,
}

#[derive(Default)]
enum HookDecision {
    #[default]
    Allow,
    Deny,
}

impl HookRunner {
    pub fn new(hooks: Vec<PluginHook>) -> Self {
        Self { hooks }
    }

    pub fn run(&self, event: HookEvent, payload: Value) -> Result<HookOutcome> {
        let mut outcome = HookOutcome::default();
        for hook in self
            .hooks
            .iter()
            .filter(|hook| hook.event == event && hook_matches(hook, &payload))
        {
            let output = run_hook(hook, event, &payload)?;
            if matches!(output.decision, HookDecision::Deny) {
                bail!(
                    "plugin '{}' denied {event:?}: {}",
                    hook.plugin,
                    output
                        .reason
                        .unwrap_or_else(|| "no reason provided".to_owned())
                );
            }
            if output.replacement.is_some() {
                outcome.replacement = output.replacement;
            }
        }
        Ok(outcome)
    }
}

fn hook_matches(hook: &PluginHook, payload: &Value) -> bool {
    let Some(matcher) = hook.matcher.as_deref() else {
        return true;
    };
    if matcher == "*" {
        return true;
    }
    let Some(name) = payload.get("name").and_then(Value::as_str) else {
        return false;
    };
    matcher.split('|').any(|candidate| candidate.trim() == name)
}

fn run_hook(hook: &PluginHook, event: HookEvent, payload: &Value) -> Result<HookOutput> {
    let root = hook
        .root
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned());
    let command = root.as_ref().map_or_else(
        || hook.command.clone(),
        |root| {
            hook.command
                .replace("${GLINT_PLUGIN_ROOT}", root)
                .replace("${CLAUDE_PLUGIN_ROOT}", root)
        },
    );
    let parts = shlex::split(&command)
        .with_context(|| format!("invalid hook command in plugin '{}'", hook.plugin))?;
    let (program, args) = parts
        .split_first()
        .with_context(|| format!("empty hook command in plugin '{}'", hook.plugin))?;
    let output_path =
        std::env::temp_dir().join(format!("glint-hook-{}.json", uuid::Uuid::new_v4()));
    let error_path = std::env::temp_dir().join(format!("glint-hook-{}.err", uuid::Uuid::new_v4()));
    let stdout = fs::File::create(&output_path)?;
    let stderr = fs::File::create(&error_path)?;
    let mut command = Command::new(program);
    command
        .args(args)
        .env("GLINT_PLUGIN", &hook.plugin)
        .env("GLINT_HOOK_EVENT", format!("{event:?}"))
        .stdin(Stdio::piped())
        .stdout(stdout)
        .stderr(stderr);
    if let Some(settings) = &hook.settings {
        command.env("GLINT_PLUGIN_SETTINGS", settings.to_string());
    }
    if let Some(root) = &hook.root {
        command
            .current_dir(root)
            .env("GLINT_PLUGIN_ROOT", root)
            .env("CLAUDE_PLUGIN_ROOT", root);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to start hook from plugin '{}'", hook.plugin))?;
    if let Some(mut stdin) = child.stdin.take() {
        serde_json::to_writer(&mut stdin, payload)?;
        stdin.flush()?;
    }

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= Duration::from_millis(hook.timeout_ms) {
            child.kill().ok();
            child.wait().ok();
            cleanup(&output_path, &error_path);
            bail!("hook from plugin '{}' timed out", hook.plugin);
        }
        thread::sleep(Duration::from_millis(20));
    };
    let stdout = fs::read_to_string(&output_path).unwrap_or_default();
    let stderr = fs::read_to_string(&error_path).unwrap_or_default();
    cleanup(&output_path, &error_path);
    if status.code() == Some(2) {
        return Ok(HookOutput {
            decision: HookDecision::Deny,
            reason: Some(stderr.trim().to_owned()),
            replacement: None,
        });
    }
    if !status.success() {
        bail!(
            "hook from plugin '{}' failed: {}",
            hook.plugin,
            stderr.trim()
        );
    }
    if stdout.trim().is_empty() {
        return Ok(HookOutput {
            decision: HookDecision::Allow,
            reason: None,
            replacement: None,
        });
    }
    let value: Value = serde_json::from_str(&stdout)
        .with_context(|| format!("hook from plugin '{}' returned invalid JSON", hook.plugin))?;
    Ok(parse_hook_output(value))
}

fn parse_hook_output(value: Value) -> HookOutput {
    let specific = value.get("hookSpecificOutput").unwrap_or(&value);
    let decision = value
        .get("decision")
        .or_else(|| specific.get("permissionDecision"))
        .and_then(Value::as_str);
    let decision = if matches!(decision, Some("deny" | "block")) {
        HookDecision::Deny
    } else {
        HookDecision::Allow
    };
    let reason = value
        .get("reason")
        .or_else(|| specific.get("permissionDecisionReason"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let replacement = value.get("replacement").cloned().or_else(|| {
        specific
            .get("updatedInput")
            .cloned()
            .map(|arguments| serde_json::json!({"arguments": arguments}))
    });
    HookOutput {
        decision,
        reason,
        replacement,
    }
}

fn cleanup(output: &std::path::Path, error: &std::path::Path) {
    fs::remove_file(output).ok();
    fs::remove_file(error).ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_can_replace_or_deny_event_payloads() {
        if Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let replace_script = script(
            "replace",
            "import os\nassert '\"mode\":\"strict\"' in os.environ['GLINT_PLUGIN_SETTINGS']\nprint('{\"decision\":\"allow\",\"replacement\":{\"prompt\":\"changed\"}}')",
        );
        let deny_script = script(
            "deny",
            "print('{\"decision\":\"deny\",\"reason\":\"blocked\"}')",
        );
        let replace = HookRunner::new(vec![PluginHook {
            event: HookEvent::PromptSubmit,
            command: format!("python3 {}", replace_script.display()),
            matcher: None,
            timeout_ms: 2_000,
            plugin: "replace".to_owned(),
            root: None,
            settings: Some(serde_json::json!({"mode":"strict"})),
        }]);
        let outcome = replace
            .run(
                HookEvent::PromptSubmit,
                serde_json::json!({"prompt":"original"}),
            )
            .unwrap();
        assert_eq!(outcome.replacement.unwrap()["prompt"], "changed");

        let deny = HookRunner::new(vec![PluginHook {
            event: HookEvent::PromptSubmit,
            command: format!("python3 {}", deny_script.display()),
            matcher: None,
            timeout_ms: 2_000,
            plugin: "deny".to_owned(),
            root: None,
            settings: None,
        }]);
        let error = deny
            .run(
                HookEvent::PromptSubmit,
                serde_json::json!({"prompt":"original"}),
            )
            .unwrap_err();
        assert!(format!("{error:#}").contains("blocked"));

        fs::remove_file(replace_script).ok();
        fs::remove_file(deny_script).ok();
    }

    fn script(label: &str, body: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("glint-hook-{label}-{}.py", uuid::Uuid::new_v4()));
        fs::write(&path, body).unwrap();
        path
    }
}
