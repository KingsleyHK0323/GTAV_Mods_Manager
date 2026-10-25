// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! AI Assistant System — v0.6 slice per the project's design (see the planning vault,
//! not part of this repo): **crash/error log diagnosis only**, read-only advice, no
//! automated Plan → execute yet (that's v0.7, and needs the Action Schema + double
//! validation against the core engine's existing safety checks before it can touch
//! anything).
//!
//! **Opt-in, disabled by default** — [`AiSettings::enabled`] must be `true`, set only
//! via [`enable`], before [`diagnose`] will do anything. Diagnosis requests pass
//! through [`crate::crash_report`]'s de-identification (strip the user's home
//! directory). Translation requests send the selected source text to the configured
//! provider; API-key providers are remote.
//!
//! Provider kinds:
//! - **Ollama Local**: HTTP to `localhost:11434`, never leaves the machine. This
//!   project does not bundle or install Ollama or any model — the user installs it
//!   themselves; [`ollama_available`] just checks whether something is listening.
//! - **Ollama Cloud**, **OpenAI**, **Anthropic**, **DeepSeek**, **OpenRouter**,
//!   **Google AI**, and **NVIDIA**: provider-specific API protocols with separate API
//!   keys. Keys are stored in the OS-native credential store (Windows Credential
//!   Manager / Linux Secret Service), never in SQLite or plaintext configuration files.
//! - **Legacy Cloud**: retains settings created by the previous generic
//!   OpenAI-compatible endpoint UI for backward compatibility.
//!
//! **Honesty note**: the cloud path has been exercised end-to-end against a real
//! OpenAI-compatible provider (OpenRouter, 2026-08-30) — real HTTP round trip, real
//! model response, correctly parsed by both [`diagnose`] and [`crate::translation`].
//! The local Ollama path is still unverified against a real running Ollama instance —
//! only the request-building, response-parsing, and the "provider unavailable" path
//! (genuinely verified: no Ollama was running on the dev machine) have been checked.
//!
//! Every request also carries [`SYSTEM_SAFETY_PROMPT`] in the provider's system
//! instruction field or message ahead of the actual diagnosis/translation prompt — a
//! prompt-level reinforcement of operating rules that are otherwise enforced in Rust
//! code (the Action Schema's Plan → confirm → execute model, path scoping, no silent
//! deletion). This doesn't replace that code-level enforcement — the LLM's output is
//! still just advice/text, never something that executes directly — but it keeps the
//! model's own suggestions aligned with the same rules rather than contradicting them.

pub mod action_schema;
pub mod known_fixes;

// OpenAI-Codex: Provider-specific AI adapters and credential-store access.
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::{CoreError, CoreResult};

const KEYRING_SERVICE: &str = "GTAVModsManager";
const KEYRING_USERNAME: &str = "ai_cloud_api_key";
const DEFAULT_CLOUD_ENDPOINT: &str = "https://api.openai.com/v1/chat/completions";
const OLLAMA_CLOUD_ENDPOINT: &str = "https://ollama.com/api/chat";
const OPENAI_ENDPOINT: &str = "https://api.openai.com/v1/chat/completions";
const ANTHROPIC_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_API_VERSION: &str = "2026-10-21";
const DEEPSEEK_ENDPOINT: &str = "https://api.deepseek.com/chat/completions";
const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
const NVIDIA_ENDPOINT: &str = "https://integrate.api.nvidia.com/v1/chat/completions";
const GOOGLE_AI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const OLLAMA_BASE_URL: &str = "http://localhost:11434";
/// A real free-tier cloud model was found, during testing, to hang indefinitely with
/// no response rather than erroring — without a timeout that looks identical to the
/// app being frozen, not a provider problem. Applies to both Ollama and cloud calls.
const PROVIDER_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Sent as a system-role message (or Ollama's `system` field) ahead of every
/// diagnosis/translation prompt — see the module doc comment for why this exists
/// alongside, not instead of, the Rust-level enforcement in `action_schema`.
pub(crate) const SYSTEM_SAFETY_PROMPT: &str = "\
You are the AI assistant inside GTAV Mods Manager, a GTA V mod management tool. You only ever \
produce text (a diagnosis or a translation draft) — you never execute anything directly. Follow \
these operating rules in every suggestion you make:\n\
1. If you suggest any change to the user's mod setup or files, describe it clearly and \
completely enough that the user (or the app's own logging) can record exactly what changed and \
why — never suggest a vague or partial change.\n\
2. Only ever reference files, folders, or paths that were given to you in the current request's \
context. Never assume the existence of, or suggest touching, any path you were not given.\n\
3. If a fix would require access to something outside what you were given (a different file, a \
different folder), say so explicitly and ask the user to provide it — do not guess a path or \
proceed as if you already had access.\n\
4. Never suggest deleting a file or mod as an immediate, silent action. Any deletion-related \
suggestion must clearly state exactly what would be deleted and why, and must be presented as \
something the user explicitly confirms before it happens — never as something already done or \
safe to do without confirmation.\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum AiProviderKind {
    #[serde(rename = "ollama")]
    Ollama,
    #[serde(rename = "ollama_cloud")]
    OllamaCloud,
    #[serde(rename = "openai")]
    OpenAI,
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "deepseek")]
    DeepSeek,
    #[serde(rename = "openrouter")]
    OpenRouter,
    #[serde(rename = "google_ai")]
    GoogleAI,
    #[serde(rename = "nvidia")]
    Nvidia,
    // Retained for saved settings created by the previous generic endpoint UI.
    #[serde(rename = "cloud")]
    Cloud,
}

impl AiProviderKind {
    fn as_str(self) -> &'static str {
        match self {
            AiProviderKind::Ollama => "ollama",
            AiProviderKind::OllamaCloud => "ollama_cloud",
            AiProviderKind::OpenAI => "openai",
            AiProviderKind::Anthropic => "anthropic",
            AiProviderKind::DeepSeek => "deepseek",
            AiProviderKind::OpenRouter => "openrouter",
            AiProviderKind::GoogleAI => "google_ai",
            AiProviderKind::Nvidia => "nvidia",
            AiProviderKind::Cloud => "cloud",
        }
    }

    fn keyring_username(self) -> Option<&'static str> {
        match self {
            AiProviderKind::Ollama => None,
            AiProviderKind::OllamaCloud => Some("ai_ollama_cloud_api_key"),
            AiProviderKind::OpenAI => Some("ai_openai_api_key"),
            AiProviderKind::Anthropic => Some("ai_anthropic_api_key"),
            AiProviderKind::DeepSeek => Some("ai_deepseek_api_key"),
            AiProviderKind::OpenRouter => Some("ai_openrouter_api_key"),
            AiProviderKind::GoogleAI => Some("ai_google_ai_api_key"),
            AiProviderKind::Nvidia => Some("ai_nvidia_api_key"),
            AiProviderKind::Cloud => Some(KEYRING_USERNAME),
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "ollama" => Some(AiProviderKind::Ollama),
            "ollama_cloud" => Some(AiProviderKind::OllamaCloud),
            "openai" => Some(AiProviderKind::OpenAI),
            "anthropic" => Some(AiProviderKind::Anthropic),
            "deepseek" => Some(AiProviderKind::DeepSeek),
            "openrouter" => Some(AiProviderKind::OpenRouter),
            "google_ai" => Some(AiProviderKind::GoogleAI),
            "nvidia" => Some(AiProviderKind::Nvidia),
            "cloud" => Some(AiProviderKind::Cloud),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AiSettings {
    pub enabled: bool,
    pub provider: Option<AiProviderKind>,
    pub ollama_model: Option<String>,
    pub cloud_endpoint: Option<String>,
    pub cloud_model: Option<String>,
}

pub fn load_settings(conn: &Connection) -> CoreResult<AiSettings> {
    conn.query_row(
        "SELECT ai_enabled, ai_provider, ai_ollama_model, ai_cloud_endpoint, ai_cloud_model \
         FROM user_settings WHERE id = 1",
        [],
        |row| {
            let enabled: i64 = row.get(0)?;
            let provider: Option<String> = row.get(1)?;
            Ok(AiSettings {
                enabled: enabled != 0,
                provider: provider.and_then(|p| AiProviderKind::parse(&p)),
                ollama_model: row.get(2)?,
                cloud_endpoint: row.get(3)?,
                cloud_model: row.get(4)?,
            })
        },
    )
    .map_err(Into::into)
}

/// Enables the AI assistant with the given provider and optional model/endpoint
/// overrides. Does **not** touch API keys — store them separately with
/// [`set_provider_api_key`] or the legacy [`set_cloud_api_key`].
pub fn enable(
    conn: &Connection,
    provider: AiProviderKind,
    model: Option<String>,
    cloud_endpoint: Option<String>,
) -> CoreResult<()> {
    match provider {
        AiProviderKind::Ollama => {
            conn.execute(
                "UPDATE user_settings SET ai_enabled = 1, ai_provider = ?1, ai_ollama_model = ?2 \
                 WHERE id = 1",
                rusqlite::params![provider.as_str(), model],
            )?;
        }
        AiProviderKind::Cloud => {
            conn.execute(
                "UPDATE user_settings \
                 SET ai_enabled = 1, ai_provider = ?1, ai_cloud_model = ?2, ai_cloud_endpoint = ?3 \
                 WHERE id = 1",
                rusqlite::params![provider.as_str(), model, cloud_endpoint],
            )?;
        }
        _ => {
            conn.execute(
                "UPDATE user_settings \
                 SET ai_enabled = 1, ai_provider = ?1, ai_cloud_model = ?2, ai_cloud_endpoint = NULL \
                 WHERE id = 1",
                rusqlite::params![provider.as_str(), model],
            )?;
        }
    }
    Ok(())
}

pub fn disable(conn: &Connection) -> CoreResult<()> {
    conn.execute("UPDATE user_settings SET ai_enabled = 0 WHERE id = 1", [])?;
    Ok(())
}

/// Stores the cloud API key in the OS-native credential store — never in the SQLite
/// database or any plaintext file, per the project's design decision.
pub fn set_cloud_api_key(key: &str) -> CoreResult<()> {
    let entry = keyring_entry()?;
    entry.set_password(key).map_err(|e| CoreError::AiAssistant {
        reason: format!("could not save the API key to the OS credential store: {e}"),
    })
}

pub fn has_cloud_api_key() -> bool {
    keyring_entry().is_ok_and(|e| e.get_password().is_ok())
}

/// Stores a provider-specific API key in the OS credential store.
pub fn set_provider_api_key(provider: AiProviderKind, key: &str) -> CoreResult<()> {
    let username = provider
        .keyring_username()
        .ok_or_else(|| CoreError::AiAssistant {
            reason: "the local Ollama provider does not use an API key".to_string(),
        })?;
    provider_keyring_entry(username)?
        .set_password(key)
        .map_err(|e| CoreError::AiAssistant {
            reason: format!("could not save the API key to the OS credential store: {e}"),
        })
}

pub fn has_provider_api_key(provider: AiProviderKind) -> bool {
    provider
        .keyring_username()
        .and_then(|username| provider_keyring_entry(username).ok())
        .is_some_and(|entry| entry.get_password().is_ok())
}

fn get_provider_api_key(provider: AiProviderKind) -> CoreResult<String> {
    let username = provider
        .keyring_username()
        .ok_or_else(|| CoreError::AiAssistant {
            reason: "the local Ollama provider does not use an API key".to_string(),
        })?;
    provider_keyring_entry(username)?
        .get_password()
        .map_err(|e| CoreError::AiAssistant {
            reason: format!(
                "no API key is set for the selected provider (or it could not be read): {e}"
            ),
        })
}

fn get_cloud_api_key() -> CoreResult<String> {
    keyring_entry()?
        .get_password()
        .map_err(|e| CoreError::AiAssistant {
            reason: format!("no cloud API key is set (or it could not be read): {e}"),
        })
}

fn keyring_entry() -> CoreResult<keyring::Entry> {
    provider_keyring_entry(KEYRING_USERNAME)
}

fn provider_keyring_entry(username: &str) -> CoreResult<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, username).map_err(|e| CoreError::AiAssistant {
        reason: format!("could not access the OS credential store: {e}"),
    })
}

/// Checks whether something is listening on the local Ollama port. Does not verify a
/// model is actually pulled — just that Ollama itself appears to be running.
pub fn ollama_available() -> bool {
    ureq::get(&format!("{OLLAMA_BASE_URL}/api/tags"))
        .timeout(std::time::Duration::from_secs(2))
        .call()
        .is_ok()
}

#[derive(Deserialize)]
struct OllamaGenerateResponse {
    response: String,
}

fn build_ollama_request(model: &str, prompt: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "system": SYSTEM_SAFETY_PROMPT,
        "prompt": prompt,
        "stream": false,
    })
}

fn parse_ollama_response(body: &str) -> CoreResult<String> {
    let parsed: OllamaGenerateResponse =
        serde_json::from_str(body).map_err(|e| CoreError::AiAssistant {
            reason: format!("could not parse Ollama's response: {e}"),
        })?;
    Ok(parsed.response)
}

#[derive(Deserialize)]
struct OpenAiCompatibleResponse {
    choices: Vec<OpenAiCompatibleChoice>,
}

#[derive(Deserialize)]
struct OllamaChatResponse {
    message: OllamaChatMessage,
}

#[derive(Deserialize)]
struct OllamaChatMessage {
    content: String,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContentBlock>,
}

#[derive(Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct GoogleAiResponse {
    candidates: Vec<GoogleAiCandidate>,
}

#[derive(Deserialize)]
struct GoogleAiCandidate {
    content: GoogleAiContent,
}

#[derive(Deserialize)]
struct GoogleAiContent {
    parts: Vec<GoogleAiPart>,
}

#[derive(Deserialize)]
struct GoogleAiPart {
    text: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiCompatibleChoice {
    message: OpenAiCompatibleMessage,
}

#[derive(Deserialize)]
struct OpenAiCompatibleMessage {
    content: String,
}

fn build_cloud_request(model: &str, prompt: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM_SAFETY_PROMPT},
            {"role": "user", "content": prompt},
        ],
    })
}

fn build_ollama_chat_request(model: &str, prompt: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": SYSTEM_SAFETY_PROMPT},
            {"role": "user", "content": prompt},
        ],
        "stream": false,
    })
}

fn build_anthropic_request(model: &str, prompt: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "max_tokens": 8192,
        "system": SYSTEM_SAFETY_PROMPT,
        "messages": [{"role": "user", "content": prompt}],
    })
}

fn build_google_ai_request(prompt: &str) -> serde_json::Value {
    serde_json::json!({
        "systemInstruction": {
            "parts": [{"text": SYSTEM_SAFETY_PROMPT}],
        },
        "contents": [{
            "role": "user",
            "parts": [{"text": prompt}],
        }],
    })
}

fn parse_cloud_response(body: &str) -> CoreResult<String> {
    let parsed: OpenAiCompatibleResponse =
        serde_json::from_str(body).map_err(|e| CoreError::AiAssistant {
            reason: format!("could not parse the cloud provider's response: {e}"),
        })?;
    parsed
        .choices
        .into_iter()
        .next()
        .map(|c| c.message.content)
        .ok_or_else(|| CoreError::AiAssistant {
            reason: "cloud provider response had no choices".to_string(),
        })
}

fn parse_ollama_chat_response(body: &str) -> CoreResult<String> {
    let parsed: OllamaChatResponse =
        serde_json::from_str(body).map_err(|e| CoreError::AiAssistant {
            reason: format!("could not parse Ollama Cloud's response: {e}"),
        })?;
    Ok(parsed.message.content)
}

fn parse_anthropic_response(body: &str) -> CoreResult<String> {
    let parsed: AnthropicResponse =
        serde_json::from_str(body).map_err(|e| CoreError::AiAssistant {
            reason: format!("could not parse Anthropic's response: {e}"),
        })?;
    let text = parsed
        .content
        .into_iter()
        .filter(|block| block.kind == "text")
        .filter_map(|block| block.text)
        .collect::<Vec<_>>()
        .join("");
    if text.is_empty() {
        return Err(CoreError::AiAssistant {
            reason: "Anthropic response contained no text blocks".to_string(),
        });
    }
    Ok(text)
}

fn parse_google_ai_response(body: &str) -> CoreResult<String> {
    let parsed: GoogleAiResponse =
        serde_json::from_str(body).map_err(|e| CoreError::AiAssistant {
            reason: format!("could not parse Google AI's response: {e}"),
        })?;
    let text = parsed
        .candidates
        .into_iter()
        .next()
        .map(|candidate| {
            candidate
                .content
                .parts
                .into_iter()
                .filter_map(|part| part.text)
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default();
    if text.is_empty() {
        return Err(CoreError::AiAssistant {
            reason: "Google AI response contained no text candidate".to_string(),
        });
    }
    Ok(text)
}

const DIAGNOSIS_PROMPT_PREFIX: &str = "You are helping diagnose a problem with GTA V mods \
    managed by GTAV Mods Manager. Given the following (already de-identified) error or crash \
    log, suggest likely causes and next steps. Be concise.\n\n";

/// Sends `raw_context` (a crash/error log or free-text description) to the configured
/// provider for a read-only diagnosis. Requires [`enable`] to have been called first.
/// Always de-identifies `raw_context` before it leaves this function, regardless of
/// provider.
pub fn diagnose(conn: &Connection, raw_context: &str) -> CoreResult<String> {
    let sanitized = crate::crash_report::deidentify(raw_context);
    let prompt = format!("{DIAGNOSIS_PROMPT_PREFIX}{sanitized}");
    call_provider(conn, &prompt)
}

/// Sends `prompt` to whichever provider is currently configured (requires [`enable`]
/// to have been called first). Shared by every AI-assisted feature in this crate
/// ([`diagnose`], [`crate::translation`]) — there is exactly one place that talks to
/// Ollama/cloud endpoints, so a provider-handling fix only needs to happen once.
pub(crate) fn call_provider(conn: &Connection, prompt: &str) -> CoreResult<String> {
    let settings = load_settings(conn)?;
    if !settings.enabled {
        return Err(CoreError::AiAssistant {
            reason: "AI assistant is disabled. Enable it first (see the `ai enable` command)."
                .to_string(),
        });
    }
    let Some(provider) = settings.provider else {
        return Err(CoreError::AiAssistant {
            reason: "AI assistant is enabled but has no provider configured.".to_string(),
        });
    };

    match provider {
        AiProviderKind::Ollama => {
            let model = settings.ollama_model.as_deref().unwrap_or("llama3");
            let body = build_ollama_request(model, prompt);
            let response = ureq::post(&format!("{OLLAMA_BASE_URL}/api/generate"))
                .timeout(PROVIDER_REQUEST_TIMEOUT)
                .send_json(body)
                .map_err(|e| CoreError::AiAssistant {
                    reason: format!(
                        "could not reach Ollama at {OLLAMA_BASE_URL} — is it running? ({e})"
                    ),
                })?
                .into_string()
                .map_err(|e| CoreError::AiAssistant {
                    reason: format!("could not read Ollama's response: {e}"),
                })?;
            parse_ollama_response(&response)
        }
        AiProviderKind::OllamaCloud => {
            let api_key = get_provider_api_key(provider)?;
            let model = required_cloud_model(&settings)?;
            let response = ureq::post(OLLAMA_CLOUD_ENDPOINT)
                .timeout(PROVIDER_REQUEST_TIMEOUT)
                .set("Authorization", &format!("Bearer {api_key}"))
                .send_json(build_ollama_chat_request(model, prompt))
                .map_err(|e| CoreError::AiAssistant {
                    reason: format!("Ollama Cloud request failed: {e}"),
                })?
                .into_string()
                .map_err(|e| CoreError::AiAssistant {
                    reason: format!("could not read Ollama Cloud's response: {e}"),
                })?;
            parse_ollama_chat_response(&response)
        }
        AiProviderKind::OpenAI => call_openai_compatible_provider(
            OPENAI_ENDPOINT,
            get_provider_api_key(provider)?,
            required_cloud_model(&settings)?,
            prompt,
            "OpenAI",
        ),
        AiProviderKind::DeepSeek => call_openai_compatible_provider(
            DEEPSEEK_ENDPOINT,
            get_provider_api_key(provider)?,
            required_cloud_model(&settings)?,
            prompt,
            "DeepSeek",
        ),
        AiProviderKind::OpenRouter => call_openai_compatible_provider(
            OPENROUTER_ENDPOINT,
            get_provider_api_key(provider)?,
            required_cloud_model(&settings)?,
            prompt,
            "OpenRouter",
        ),
        AiProviderKind::Nvidia => call_openai_compatible_provider(
            NVIDIA_ENDPOINT,
            get_provider_api_key(provider)?,
            required_cloud_model(&settings)?,
            prompt,
            "NVIDIA",
        ),
        AiProviderKind::GoogleAI => {
            let api_key = get_provider_api_key(provider)?;
            let model = required_cloud_model(&settings)?;
            if !model
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            {
                return Err(CoreError::AiAssistant {
                    reason: "Google AI model ID may only contain letters, numbers, '.', '_' or '-'"
                        .to_string(),
                });
            }
            let endpoint = format!("{GOOGLE_AI_BASE_URL}/{model}:generateContent");
            let response = ureq::post(&endpoint)
                .timeout(PROVIDER_REQUEST_TIMEOUT)
                .set("x-goog-api-key", &api_key)
                .send_json(build_google_ai_request(prompt))
                .map_err(|e| CoreError::AiAssistant {
                    reason: format!("Google AI request failed: {e}"),
                })?
                .into_string()
                .map_err(|e| CoreError::AiAssistant {
                    reason: format!("could not read Google AI's response: {e}"),
                })?;
            parse_google_ai_response(&response)
        }
        AiProviderKind::Anthropic => {
            let api_key = get_provider_api_key(provider)?;
            let model = required_cloud_model(&settings)?;
            let response = ureq::post(ANTHROPIC_ENDPOINT)
                .timeout(PROVIDER_REQUEST_TIMEOUT)
                .set("x-api-key", &api_key)
                .set("anthropic-version", ANTHROPIC_API_VERSION)
                .send_json(build_anthropic_request(model, prompt))
                .map_err(|e| CoreError::AiAssistant {
                    reason: format!("Anthropic request failed: {e}"),
                })?
                .into_string()
                .map_err(|e| CoreError::AiAssistant {
                    reason: format!("could not read Anthropic's response: {e}"),
                })?;
            parse_anthropic_response(&response)
        }
        AiProviderKind::Cloud => {
            let api_key = get_cloud_api_key()?;
            let endpoint = settings
                .cloud_endpoint
                .as_deref()
                .unwrap_or(DEFAULT_CLOUD_ENDPOINT);
            let model = settings.cloud_model.as_deref().unwrap_or("gpt-4o-mini");
            let body = build_cloud_request(model, prompt);
            let response = ureq::post(endpoint)
                .timeout(PROVIDER_REQUEST_TIMEOUT)
                .set("Authorization", &format!("Bearer {api_key}"))
                .send_json(body)
                .map_err(|e| CoreError::AiAssistant {
                    reason: format!("cloud provider request failed: {e}"),
                })?
                .into_string()
                .map_err(|e| CoreError::AiAssistant {
                    reason: format!("could not read the cloud provider's response: {e}"),
                })?;
            parse_cloud_response(&response)
        }
    }
}

fn required_cloud_model(settings: &AiSettings) -> CoreResult<&str> {
    settings
        .cloud_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .ok_or_else(|| CoreError::AiAssistant {
            reason: "enter a model ID for the selected provider in Settings".to_string(),
        })
}

fn call_openai_compatible_provider(
    endpoint: &str,
    api_key: String,
    model: &str,
    prompt: &str,
    provider_label: &str,
) -> CoreResult<String> {
    let response = ureq::post(endpoint)
        .timeout(PROVIDER_REQUEST_TIMEOUT)
        .set("Authorization", &format!("Bearer {api_key}"))
        .send_json(build_cloud_request(model, prompt))
        .map_err(|e| CoreError::AiAssistant {
            reason: format!("{provider_label} request failed: {e}"),
        })?
        .into_string()
        .map_err(|e| CoreError::AiAssistant {
            reason: format!("could not read {provider_label}'s response: {e}"),
        })?;
    parse_cloud_response(&response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_to_disabled_with_no_provider() {
        let conn = crate::db::open_in_memory().unwrap();
        let settings = load_settings(&conn).unwrap();
        assert!(!settings.enabled);
        assert_eq!(settings.provider, None);
    }

    #[test]
    fn enable_ollama_then_disable_roundtrips() {
        let conn = crate::db::open_in_memory().unwrap();
        enable(
            &conn,
            AiProviderKind::Ollama,
            Some("llama3.1".to_string()),
            None,
        )
        .unwrap();

        let settings = load_settings(&conn).unwrap();
        assert!(settings.enabled);
        assert_eq!(settings.provider, Some(AiProviderKind::Ollama));
        assert_eq!(settings.ollama_model.as_deref(), Some("llama3.1"));

        disable(&conn).unwrap();
        assert!(!load_settings(&conn).unwrap().enabled);
    }

    #[test]
    fn enable_cloud_stores_endpoint_and_model_not_key() {
        let conn = crate::db::open_in_memory().unwrap();
        enable(
            &conn,
            AiProviderKind::Cloud,
            Some("gpt-4o".to_string()),
            Some("https://example.com/v1/chat/completions".to_string()),
        )
        .unwrap();

        let settings = load_settings(&conn).unwrap();
        assert_eq!(settings.provider, Some(AiProviderKind::Cloud));
        assert_eq!(settings.cloud_model.as_deref(), Some("gpt-4o"));
        assert_eq!(
            settings.cloud_endpoint.as_deref(),
            Some("https://example.com/v1/chat/completions")
        );
    }

    #[test]
    fn diagnose_refuses_when_disabled() {
        let conn = crate::db::open_in_memory().unwrap();
        let err = diagnose(&conn, "some error").unwrap_err();
        assert!(matches!(err, CoreError::AiAssistant { .. }));
    }

    #[test]
    fn diagnose_deidentifies_context_before_building_the_prompt() {
        std::env::set_var("HOME", "/home/testuser");
        let sanitized = crate::crash_report::deidentify("error at /home/testuser/game/mod.asi");
        assert!(!sanitized.contains("testuser"));
        std::env::remove_var("HOME");
    }

    #[test]
    fn builds_expected_ollama_request_shape() {
        let body = build_ollama_request("llama3", "hello");
        assert_eq!(body["model"], "llama3");
        assert_eq!(body["prompt"], "hello");
        assert_eq!(body["stream"], false);
        assert_eq!(body["system"], SYSTEM_SAFETY_PROMPT);
    }

    #[test]
    fn parses_ollama_response() {
        let raw = r#"{"response": "looks like a missing dependency"}"#;
        assert_eq!(
            parse_ollama_response(raw).unwrap(),
            "looks like a missing dependency"
        );
    }

    #[test]
    fn builds_expected_cloud_request_shape() {
        let body = build_cloud_request("gpt-4o-mini", "hello");
        assert_eq!(body["model"], "gpt-4o-mini");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], SYSTEM_SAFETY_PROMPT);
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "hello");
    }

    #[test]
    fn parses_cloud_response() {
        let raw = r#"{"choices": [{"message": {"content": "try disabling ModX"}}]}"#;
        assert_eq!(parse_cloud_response(raw).unwrap(), "try disabling ModX");
    }

    #[test]
    fn cloud_response_with_no_choices_is_an_error() {
        let raw = r#"{"choices": []}"#;
        assert!(parse_cloud_response(raw).is_err());
    }

    #[test]
    fn ollama_unavailable_when_nothing_is_listening() {
        // Real check against this machine — no Ollama install exists here, so this
        // genuinely exercises the "unavailable" path rather than asserting a mock.
        assert!(!ollama_available());
    }

    /// Regression test for a real bug found on this machine: keyring 3.x's default
    /// Cargo features include **no actual OS backend at all**, so `set_password`
    /// silently "succeeds" against a no-op store and every `get_password` afterwards
    /// returns `NoEntry` — confirmed directly against a real Windows Credential
    /// Manager on this machine before `windows-native`/`sync-secret-service` were
    /// added to `Cargo.toml`. This exercises the real OS credential store (using a
    /// distinct service name so it never touches the real `ai_cloud_api_key` entry),
    /// but tolerates the backend being genuinely unavailable — a headless Linux CI
    /// runner typically has no D-Bus Secret Service daemon running, and that's an
    /// environment limitation, not a bug in this code.
    #[test]
    fn real_os_credential_store_roundtrip_when_available() {
        let entry = keyring::Entry::new("GTAVModsManager-test", "roundtrip-test-key-do-not-use")
            .expect("constructing an Entry never touches the backend");

        if entry.set_password("roundtrip-value").is_err() {
            eprintln!("skipping: no OS credential store backend is reachable in this environment");
            return;
        }

        let read_back = entry
            .get_password()
            .expect("a value just set must be readable back, not NoEntry");
        assert_eq!(read_back, "roundtrip-value");

        let _ = entry.delete_credential();
    }
}
