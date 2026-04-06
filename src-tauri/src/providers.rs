use crate::models::{
    AppSettings, ProviderConfig, ProviderSettings, SaveProviderRequest, WorkflowOptions,
};
use crate::util::strip_json_fence;
use crate::vault;
use anyhow::{Context, Result, anyhow};
use keyring::{Entry, Error as KeyringError};
use reqwest::blocking::Client;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tempfile::NamedTempFile;

const KEYCHAIN_SERVICE: &str = "com.tanush.wiki";

#[derive(Debug, Clone)]
pub struct ProviderExecutionMetadata {
    pub provider_id: String,
    pub provider_kind: String,
    pub provider_mode: String,
    pub execution_mode: String,
    pub stderr: Option<String>,
}

impl ProviderExecutionMetadata {
    pub fn as_json(&self) -> Value {
        json!({
            "provider_id": self.provider_id,
            "provider_kind": self.provider_kind,
            "provider_mode": self.provider_mode,
            "execution_mode": self.execution_mode,
            "stderr": self.stderr,
        })
    }
}

#[derive(Debug, Clone)]
pub struct TextExecutionResult {
    pub text: String,
    pub metadata: ProviderExecutionMetadata,
}

#[derive(Debug, Clone)]
pub struct StructuredExecutionResult<T> {
    pub value: T,
    pub metadata: ProviderExecutionMetadata,
}

#[derive(Debug, Clone)]
struct ResolvedProvider {
    config: ProviderConfig,
    provider_mode: String,
    execution_mode: String,
    secret: Option<String>,
    executable_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct LocalProviderHealth {
    available: bool,
    configured: bool,
    auth_status: String,
    health_message: Option<String>,
    executable_path: Option<String>,
    supports_direct_edit: bool,
}

#[derive(Debug)]
struct LocalCommandOutput {
    text: String,
    stderr: Option<String>,
}

pub fn provider_statuses(root: &Path) -> Result<Vec<ProviderSettings>> {
    let settings = vault::load_settings(root)?;
    settings
        .providers
        .iter()
        .map(|provider| provider_status(provider, &settings))
        .collect()
}

pub fn save_provider(root: &Path, request: SaveProviderRequest) -> Result<Vec<ProviderSettings>> {
    let mut settings = vault::load_settings(root)?;
    if let Some(provider) = settings
        .providers
        .iter_mut()
        .find(|provider| provider.id == request.id)
    {
        provider.model = request.model.clone();
        provider.base_url = request.base_url.clone();
    } else {
        settings.providers.push(ProviderConfig {
            id: request.id.clone(),
            label: request.id.clone(),
            kind: "openai-compatible".to_string(),
            base_url: request.base_url.clone(),
            model: request.model.clone(),
            supports_embeddings: false,
        });
    }

    if let Some(api_key) = request.api_key.as_ref() {
        let entry = Entry::new(KEYCHAIN_SERVICE, &request.id)?;
        if api_key.trim().is_empty() {
            let _ = entry.delete_password();
        } else {
            entry
                .set_password(api_key)
                .with_context(|| format!("failed to save key for {}", request.id))?;
        }
    }

    if request.selected {
        if let Some(provider) = settings.providers.iter().find(|provider| provider.id == request.id) {
            if is_subscription_kind(&provider.kind) {
                settings.selected_subscription_provider = Some(provider.id.clone());
            } else {
                settings.selected_api_provider = Some(provider.id.clone());
                settings.selected_provider = Some(provider.id.clone());
            }
        }
    }

    vault::save_settings(root, &settings)?;
    provider_statuses(root)
}

pub fn chat(root: &Path, options: WorkflowOptions, system: &str, user: &str) -> Result<TextExecutionResult> {
    let provider = resolve_provider(root, &options)?;
    match provider.config.kind.as_str() {
        "anthropic" => {
            let secret = provider
                .secret
                .as_deref()
                .ok_or_else(|| anyhow!("missing API key for {}", provider.config.label))?;
            let text = anthropic_chat(&provider.config, secret, system, user)?;
            Ok(TextExecutionResult {
                text,
                metadata: provider_metadata(&provider, None),
            })
        }
        "openai" | "openai-compatible" => {
            let secret = provider
                .secret
                .as_deref()
                .ok_or_else(|| anyhow!("missing API key for {}", provider.config.label))?;
            let text = openai_like_chat(&provider.config, secret, system, user)?;
            Ok(TextExecutionResult {
                text,
                metadata: provider_metadata(&provider, None),
            })
        }
        "claude-code-cli" => {
            let executable = provider
                .executable_path
                .as_deref()
                .ok_or_else(|| anyhow!("claude executable not found"))?;
            let output = run_claude_print(executable, root, &provider.config.model, system, user, None, false)?;
            Ok(TextExecutionResult {
                text: output.text,
                metadata: provider_metadata(&provider, output.stderr),
            })
        }
        "codex-cli" => {
            let executable = provider
                .executable_path
                .as_deref()
                .ok_or_else(|| anyhow!("codex executable not found"))?;
            let output = run_codex_review(executable, root, &provider.config.model, &combined_prompt(system, user), None)?;
            Ok(TextExecutionResult {
                text: output.text,
                metadata: provider_metadata(&provider, output.stderr),
            })
        }
        other => Err(anyhow!("unsupported provider kind: {other}")),
    }
}

pub fn structured_json<T: DeserializeOwned>(
    root: &Path,
    options: WorkflowOptions,
    system: &str,
    user: &str,
    schema_hint: &str,
    json_schema: &Value,
) -> Result<StructuredExecutionResult<T>> {
    let wrapped_user = format!(
        "{user}\n\nReturn only valid JSON. Do not include markdown fences.\nExpected shape:\n{schema_hint}"
    );
    let provider = resolve_provider(root, &options)?;
    let output = match provider.config.kind.as_str() {
        "anthropic" => {
            let secret = provider
                .secret
                .as_deref()
                .ok_or_else(|| anyhow!("missing API key for {}", provider.config.label))?;
            LocalCommandOutput {
                text: anthropic_chat(&provider.config, secret, system, &wrapped_user)?,
                stderr: None,
            }
        }
        "openai" | "openai-compatible" => {
            let secret = provider
                .secret
                .as_deref()
                .ok_or_else(|| anyhow!("missing API key for {}", provider.config.label))?;
            LocalCommandOutput {
                text: openai_like_chat(&provider.config, secret, system, &wrapped_user)?,
                stderr: None,
            }
        }
        "claude-code-cli" => {
            let executable = provider
                .executable_path
                .as_deref()
                .ok_or_else(|| anyhow!("claude executable not found"))?;
            run_claude_print(
                executable,
                root,
                &provider.config.model,
                system,
                &wrapped_user,
                Some(json_schema),
                false,
            )?
        }
        "codex-cli" => {
            let executable = provider
                .executable_path
                .as_deref()
                .ok_or_else(|| anyhow!("codex executable not found"))?;
            run_codex_review(
                executable,
                root,
                &provider.config.model,
                &combined_prompt(system, &wrapped_user),
                Some(json_schema),
            )?
        }
        other => return Err(anyhow!("unsupported provider kind: {other}")),
    };

    let json_body = strip_json_fence(&output.text);
    let value = serde_json::from_str(json_body)
        .with_context(|| format!("failed to parse model JSON: {json_body}"))?;
    Ok(StructuredExecutionResult {
        value,
        metadata: provider_metadata(&provider, output.stderr),
    })
}

pub fn direct_edit(
    root: &Path,
    options: WorkflowOptions,
    system: &str,
    user: &str,
) -> Result<TextExecutionResult> {
    let mut provider = resolve_provider(root, &options)?;
    if !is_subscription_kind(&provider.config.kind) {
        return Err(anyhow!("direct edit requires a subscription agent provider"));
    }
    provider.execution_mode = "direct-edit".to_string();
    let executable = provider
        .executable_path
        .as_deref()
        .ok_or_else(|| anyhow!("provider executable not found"))?;
    let output = match provider.config.kind.as_str() {
        "claude-code-cli" => {
            run_claude_print(executable, root, &provider.config.model, system, user, None, true)?
        }
        "codex-cli" => {
            run_codex_direct_edit(
                executable,
                root,
                &provider.config.model,
                &combined_prompt(system, user),
            )?
        }
        other => return Err(anyhow!("direct edit is not supported for {other}")),
    };

    Ok(TextExecutionResult {
        text: output.text,
        metadata: provider_metadata(&provider, output.stderr),
    })
}

fn provider_status(provider: &ProviderConfig, settings: &AppSettings) -> Result<ProviderSettings> {
    let selected = if is_subscription_kind(&provider.kind) {
        settings.selected_subscription_provider.as_deref() == Some(provider.id.as_str())
    } else {
        settings.selected_api_provider.as_deref() == Some(provider.id.as_str())
    };

    if is_subscription_kind(&provider.kind) {
        let health = local_provider_health(provider)?;
        return Ok(ProviderSettings {
            id: provider.id.clone(),
            label: provider.label.clone(),
            kind: provider.kind.clone(),
            base_url: provider.base_url.clone(),
            model: provider.model.clone(),
            configured: health.configured,
            selected,
            supports_embeddings: provider.supports_embeddings,
            available: health.available,
            auth_status: health.auth_status,
            health_message: health.health_message,
            executable_path: health.executable_path,
            supports_direct_edit: health.supports_direct_edit,
        });
    }

    let configured = has_secret(&provider.id)?;
    Ok(ProviderSettings {
        id: provider.id.clone(),
        label: provider.label.clone(),
        kind: provider.kind.clone(),
        base_url: provider.base_url.clone(),
        model: provider.model.clone(),
        configured,
        selected,
        supports_embeddings: provider.supports_embeddings,
        available: true,
        auth_status: if configured {
            "configured".to_string()
        } else {
            "missing-api-key".to_string()
        },
        health_message: Some(if configured {
            "API key saved in macOS Keychain.".to_string()
        } else {
            "Save an API key to use this provider.".to_string()
        }),
        executable_path: None,
        supports_direct_edit: false,
    })
}

fn resolve_provider(root: &Path, options: &WorkflowOptions) -> Result<ResolvedProvider> {
    let settings = vault::load_settings(root)?;
    let provider_mode = normalize_provider_mode(
        options
            .provider_mode
            .as_deref()
            .unwrap_or(settings.provider_mode.as_str()),
    );
    let mut execution_mode = normalize_execution_mode(
        options
            .execution_mode
            .as_deref()
            .unwrap_or(settings.default_execution_mode.as_str()),
    );

    let provider = resolve_provider_config(&settings, &provider_mode, options.provider_id.clone())?;
    if provider_mode == "api" && execution_mode == "direct-edit" {
        execution_mode = "review-first".to_string();
    }

    if is_subscription_kind(&provider.kind) {
        let health = local_provider_health(&provider)?;
        if !health.available {
            return Err(anyhow!(
                health
                    .health_message
                    .unwrap_or_else(|| format!("{} is unavailable", provider.label))
            ));
        }
        if !health.configured {
            return Err(anyhow!(
                health
                    .health_message
                    .unwrap_or_else(|| format!("{} is not authenticated", provider.label))
            ));
        }
        return Ok(ResolvedProvider {
            config: provider,
            provider_mode,
            execution_mode,
            secret: None,
            executable_path: health.executable_path.map(PathBuf::from),
        });
    }

    let secret = read_secret(&provider.id)?
        .ok_or_else(|| anyhow!("no API key saved for {}", provider.label))?;
    Ok(ResolvedProvider {
        config: provider,
        provider_mode,
        execution_mode,
        secret: Some(secret),
        executable_path: None,
    })
}

fn resolve_provider_config(
    settings: &AppSettings,
    provider_mode: &str,
    requested_provider_id: Option<String>,
) -> Result<ProviderConfig> {
    let selected_id = requested_provider_id.or_else(|| {
        if provider_mode == "subscription" {
            settings.selected_subscription_provider.clone()
        } else {
            settings
                .selected_api_provider
                .clone()
                .or(settings.selected_provider.clone())
        }
    });

    if let Some(selected_id) = selected_id {
        let provider = settings
            .providers
            .iter()
            .find(|provider| provider.id == selected_id)
            .cloned()
            .ok_or_else(|| anyhow!("provider `{selected_id}` was not found"))?;
        if provider_mode == "subscription" && !is_subscription_kind(&provider.kind) {
            return Err(anyhow!("provider `{selected_id}` is not a subscription agent"));
        }
        if provider_mode == "api" && is_subscription_kind(&provider.kind) {
            return Err(anyhow!("provider `{selected_id}` is not an API provider"));
        }
        return Ok(provider);
    }

    settings
        .providers
        .iter()
        .find(|provider| {
            if provider_mode == "subscription" {
                is_subscription_kind(&provider.kind)
            } else {
                !is_subscription_kind(&provider.kind)
            }
        })
        .cloned()
        .ok_or_else(|| anyhow!("no provider configuration found for mode `{provider_mode}`"))
}

fn local_provider_health(provider: &ProviderConfig) -> Result<LocalProviderHealth> {
    match provider.kind.as_str() {
        "claude-code-cli" => claude_health(),
        "codex-cli" => codex_health(),
        other => Err(anyhow!("unsupported local provider kind: {other}")),
    }
}

fn claude_health() -> Result<LocalProviderHealth> {
    let executable_path = resolve_cli_path("claude");
    let Some(executable_path) = executable_path else {
        return Ok(LocalProviderHealth {
            available: false,
            configured: false,
            auth_status: "missing-binary".to_string(),
            health_message: Some("Install Claude Code and make `claude` available on your PATH.".to_string()),
            executable_path: None,
            supports_direct_edit: true,
        });
    };

    let version = Command::new(&executable_path).arg("--version").output();
    let binary_healthy = version.as_ref().map(|output| output.status.success()).unwrap_or(false);
    let auth = Command::new(&executable_path).args(["auth", "status"]).output();

    let (configured, auth_status, auth_message) = match auth {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            (
                true,
                "authenticated".to_string(),
                Some(if stdout.is_empty() {
                    "Claude Code is authenticated.".to_string()
                } else {
                    stdout
                }),
            )
        }
        Ok(output) => {
            let stderr = clean_output(&output.stderr);
            let stdout = clean_output(&output.stdout);
            (
                false,
                "login-required".to_string(),
                Some(if !stderr.is_empty() {
                    stderr
                } else if !stdout.is_empty() {
                    stdout
                } else {
                    "Run `claude login` to authenticate this machine.".to_string()
                }),
            )
        }
        Err(error) => (
            false,
            "login-required".to_string(),
            Some(format!("Failed to check Claude Code auth: {error}")),
        ),
    };

    Ok(LocalProviderHealth {
        available: binary_healthy,
        configured,
        auth_status,
        health_message: auth_message,
        executable_path: Some(executable_path.display().to_string()),
        supports_direct_edit: true,
    })
}

fn codex_health() -> Result<LocalProviderHealth> {
    let executable_path = resolve_cli_path("codex");
    let Some(executable_path) = executable_path else {
        return Ok(LocalProviderHealth {
            available: false,
            configured: false,
            auth_status: "missing-binary".to_string(),
            health_message: Some("Install Codex and make `codex` available on your PATH.".to_string()),
            executable_path: None,
            supports_direct_edit: true,
        });
    };

    let auth_path = env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .map(|path| path.join(".codex/auth.json"));
    let configured = auth_path.as_ref().is_some_and(|path| path.exists());
    let output = codex_command(&executable_path)?.arg("--version").output();

    let (available, health_message) = match output {
        Ok(output) if output.status.success() => (
            true,
            Some(format!(
                "Codex CLI is installed and runnable. {}",
                clean_output(&output.stdout)
            )),
        ),
        Ok(output) => {
            let stderr = clean_output(&output.stderr);
            let stdout = clean_output(&output.stdout);
            let message = if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                "Codex CLI failed to start.".to_string()
            };
            (false, Some(message))
        }
        Err(error) => (
            false,
            Some(format!("Failed to start Codex CLI: {error}")),
        ),
    };

    let auth_status = if configured {
        "authenticated".to_string()
    } else {
        "login-required".to_string()
    };
    let health_message = if !available {
        Some(format!(
            "{}{}",
            health_message.unwrap_or_else(|| "Codex CLI is unavailable.".to_string()),
            if configured {
                "".to_string()
            } else {
                " Reinstall with `npm install -g @openai/codex@latest` and sign in with ChatGPT.".to_string()
            }
        ))
    } else if configured {
        health_message
    } else {
        Some("Run `codex` and sign in with ChatGPT to use subscription-backed Codex.".to_string())
    };

    Ok(LocalProviderHealth {
        available,
        configured,
        auth_status,
        health_message,
        executable_path: Some(executable_path.display().to_string()),
        supports_direct_edit: true,
    })
}

fn provider_metadata(provider: &ResolvedProvider, stderr: Option<String>) -> ProviderExecutionMetadata {
    ProviderExecutionMetadata {
        provider_id: provider.config.id.clone(),
        provider_kind: provider.config.kind.clone(),
        provider_mode: provider.provider_mode.clone(),
        execution_mode: provider.execution_mode.clone(),
        stderr,
    }
}

fn has_secret(provider_id: &str) -> Result<bool> {
    Ok(read_secret(provider_id)?.is_some())
}

fn read_secret(provider_id: &str) -> Result<Option<String>> {
    let entry = Entry::new(KEYCHAIN_SERVICE, provider_id)?;
    match entry.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(KeyringError::NoEntry) => Ok(None),
        Err(error) => Err(anyhow!(error.to_string())).context(format!("failed to read key for {provider_id}")),
    }
}

fn openai_like_chat(provider: &ProviderConfig, api_key: &str, system: &str, user: &str) -> Result<String> {
    let base_url = provider
        .base_url
        .clone()
        .ok_or_else(|| anyhow!("missing base URL for {}", provider.label))?;
    let client = Client::builder().timeout(Duration::from_secs(90)).build()?;
    let response = client
        .post(format!("{}/chat/completions", base_url.trim_end_matches('/')))
        .bearer_auth(api_key)
        .json(&json!({
            "model": provider.model,
            "temperature": 0.2,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user}
            ]
        }))
        .send()?
        .error_for_status()?;

    let payload: Value = response.json()?;
    extract_openai_text(&payload).ok_or_else(|| anyhow!("provider response did not contain text"))
}

fn anthropic_chat(provider: &ProviderConfig, api_key: &str, system: &str, user: &str) -> Result<String> {
    let base_url = provider
        .base_url
        .clone()
        .unwrap_or_else(|| "https://api.anthropic.com/v1".to_string());
    let client = Client::builder().timeout(Duration::from_secs(90)).build()?;
    let response = client
        .post(format!("{}/messages", base_url.trim_end_matches('/')))
        .header("x-api-key", api_key)
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": provider.model,
            "max_tokens": 3000,
            "system": system,
            "messages": [
                {
                    "role": "user",
                    "content": user
                }
            ]
        }))
        .send()?
        .error_for_status()?;

    let payload: Value = response.json()?;
    extract_anthropic_text(&payload).ok_or_else(|| anyhow!("provider response did not contain text"))
}

fn run_claude_print(
    executable: &Path,
    cwd: &Path,
    model: &str,
    system: &str,
    user: &str,
    json_schema: Option<&Value>,
    direct_edit: bool,
) -> Result<LocalCommandOutput> {
    let mut command = Command::new(executable);
    command
        .current_dir(cwd)
        .arg("-p")
        .arg("--output-format")
        .arg("json")
        .arg("--permission-mode")
        .arg(if direct_edit { "acceptEdits" } else { "plan" })
        .arg("--model")
        .arg(model)
        .arg("--system-prompt")
        .arg(system);

    if let Some(json_schema) = json_schema {
        command.arg("--json-schema").arg(serde_json::to_string(json_schema)?);
    }

    let output = command.arg(user).output()?;
    if !output.status.success() {
        let message = choose_error_text(&output.stdout, &output.stderr);
        return Err(anyhow!(message));
    }

    let payload: Value = serde_json::from_slice(&output.stdout)?;
    Ok(LocalCommandOutput {
        text: extract_claude_result(&payload),
        stderr: clean_optional_output(&output.stderr),
    })
}

fn run_codex_review(
    executable: &Path,
    cwd: &Path,
    model: &str,
    prompt: &str,
    json_schema: Option<&Value>,
) -> Result<LocalCommandOutput> {
    let output_file = NamedTempFile::new()?;
    let mut command = codex_command(executable)?;
    command
        .current_dir(cwd)
        .arg("exec")
        .arg("-C")
        .arg(cwd)
        .arg("--skip-git-repo-check")
        .arg("--sandbox")
        .arg("read-only")
        .arg("--output-last-message")
        .arg(output_file.path());
    if !model.trim().is_empty() {
        command.arg("--model").arg(model);
    }

    let schema_file = if let Some(schema) = json_schema {
        let schema_file = NamedTempFile::new()?;
        fs::write(schema_file.path(), serde_json::to_string(schema)?)?;
        command.arg("--output-schema").arg(schema_file.path());
        Some(schema_file)
    } else {
        None
    };
    let _keep_schema = schema_file;

    let output = command.arg(prompt).output()?;
    if !output.status.success() {
        let message = choose_error_text(&output.stdout, &output.stderr);
        return Err(anyhow!(message));
    }

    let text = fs::read_to_string(output_file.path())
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    let final_text = if text.is_empty() {
        clean_output(&output.stdout)
    } else {
        text
    };
    Ok(LocalCommandOutput {
        text: final_text,
        stderr: clean_optional_output(&output.stderr),
    })
}

fn run_codex_direct_edit(
    executable: &Path,
    cwd: &Path,
    model: &str,
    prompt: &str,
) -> Result<LocalCommandOutput> {
    let output_file = NamedTempFile::new()?;
    let mut command = codex_command(executable)?;
    command
        .current_dir(cwd)
        .arg("exec")
        .arg("-C")
        .arg(cwd)
        .arg("--skip-git-repo-check")
        .arg("--full-auto")
        .arg("--output-last-message")
        .arg(output_file.path());
    if !model.trim().is_empty() {
        command.arg("--model").arg(model);
    }

    let output = command.arg(prompt).output()?;
    if !output.status.success() {
        let message = choose_error_text(&output.stdout, &output.stderr);
        return Err(anyhow!(message));
    }

    let text = fs::read_to_string(output_file.path())
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    let final_text = if text.is_empty() {
        clean_output(&output.stdout)
    } else {
        text
    };
    Ok(LocalCommandOutput {
        text: final_text,
        stderr: clean_optional_output(&output.stderr),
    })
}

fn resolve_cli_path(binary: &str) -> Option<PathBuf> {
    let output = Command::new("/bin/zsh")
        .arg("-lc")
        .arg(format!("command -v {binary}"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        None
    } else {
        Some(PathBuf::from(value))
    }
}

fn codex_command(executable: &Path) -> Result<Command> {
    if executable_uses_env_node(executable) {
        let node_path = resolve_cli_path("node").ok_or_else(|| {
            anyhow!(
                "Codex CLI requires `node`, but no node executable was found on the app PATH."
            )
        })?;
        let mut command = Command::new(node_path);
        command.arg(executable);
        return Ok(command);
    }

    Ok(Command::new(executable))
}

fn executable_uses_env_node(executable: &Path) -> bool {
    fs::read_to_string(executable)
        .ok()
        .and_then(|content| content.lines().next().map(str::to_string))
        .is_some_and(|line| line.contains("/usr/bin/env node"))
}

fn normalize_provider_mode(value: &str) -> String {
    match value {
        "api" => "api".to_string(),
        _ => "subscription".to_string(),
    }
}

fn normalize_execution_mode(value: &str) -> String {
    match value {
        "direct-edit" => "direct-edit".to_string(),
        _ => "review-first".to_string(),
    }
}

fn is_subscription_kind(kind: &str) -> bool {
    matches!(kind, "claude-code-cli" | "codex-cli")
}

fn combined_prompt(system: &str, user: &str) -> String {
    format!("System instructions:\n{system}\n\nTask:\n{user}")
}

fn clean_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

fn clean_optional_output(bytes: &[u8]) -> Option<String> {
    let value = clean_output(bytes);
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn choose_error_text(stdout: &[u8], stderr: &[u8]) -> String {
    let stderr = clean_output(stderr);
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = clean_output(stdout);
    if !stdout.is_empty() {
        return stdout;
    }
    "provider command failed".to_string()
}

fn extract_claude_result(payload: &Value) -> String {
    match payload.get("result") {
        Some(Value::String(text)) => text.trim().to_string(),
        Some(other) => other.to_string(),
        None => payload.to_string(),
    }
}

fn extract_openai_text(payload: &Value) -> Option<String> {
    let content = payload
        .get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("content")?;

    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }

    content.as_array().map(|parts| {
        parts
            .iter()
            .filter_map(|part| part.get("text").and_then(|text| text.as_str()))
            .collect::<Vec<_>>()
            .join("\n")
    })
}

fn extract_anthropic_text(payload: &Value) -> Option<String> {
    payload.get("content")?.as_array().map(|parts| {
        parts
            .iter()
            .filter_map(|part| part.get("text").and_then(|text| text.as_str()))
            .collect::<Vec<_>>()
            .join("\n")
    })
}
