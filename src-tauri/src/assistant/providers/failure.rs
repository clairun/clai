//! Typed provider HTTP failures.
//!
//! Every non-success HTTP response from a provider becomes a
//! [`ProviderFailure`] whose [`FailureKind`] names the one thing the user, or
//! CLAI, can do about it. A brand that reuses one status for several meanings
//! (Z.ai sends 429 for throttling, overload and an exhausted plan) is told
//! apart by its `error.code`, listed in [`error_codes`].

use std::fmt;
use std::time::Duration;

use reqwest::header::{HeaderMap, RETRY_AFTER};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use super::types::is_context_limit_error;

/// What a failed provider request asks for: one kind per action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export, export_to = "bindings.ts")]
pub enum FailureKind {
    /// Fix the API key.
    Auth,
    /// Wait: the limit resets on its own.
    UsageLimit,
    /// Top up or renew the account.
    Billing,
    /// Pick another model.
    ModelUnavailable,
    /// Try again: throttling, overload, server or network trouble.
    Transient,
    /// Shorten the conversation; CLAI compacts once on its own.
    ContextTooLong,
    /// Rephrase the prompt.
    ContentFiltered,
    /// Nothing specific: read the provider's message.
    Other,
}

impl FailureKind {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 8] = [
        Self::Auth,
        Self::UsageLimit,
        Self::Billing,
        Self::ModelUnavailable,
        Self::Transient,
        Self::ContextTooLong,
        Self::ContentFiltered,
        Self::Other,
    ];

    /// The sentence shown before the provider's own message. Only
    /// `UsageLimit` may name a limit: `src/assistant/runErrors.ts` reads any
    /// limit phrase in a run error as a usage limit.
    fn lead(self) -> Option<&'static str> {
        match self {
            Self::Auth => Some("The provider rejected the API key."),
            Self::UsageLimit => Some("Usage limit reached."),
            Self::Billing => Some("The provider account needs a top-up or a plan renewal."),
            Self::ModelUnavailable => Some("The model is not available."),
            Self::Transient => {
                Some("The provider is busy or temporarily unavailable; try again shortly.")
            }
            Self::ContextTooLong => Some("The request is too long for the model's context window."),
            Self::ContentFiltered => Some("The provider's content filter blocked the request."),
            Self::Other => None,
        }
    }
}

/// A provider's non-success HTTP response, classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFailure {
    pub kind: FailureKind,
    pub status: u16,
    /// The brand's own `error.code`, such as Z.ai's `"1310"`.
    pub provider_code: Option<String>,
    /// The provider's own message, or the whole body when it has none.
    pub message: String,
    pub retry_after: Option<Duration>,
}

impl fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = self.status;
        let message = &self.message;
        match (self.kind.lead(), message.is_empty()) {
            (Some(lead), false) => write!(f, "HTTP {status}: {lead} {message}"),
            (Some(lead), true) => write!(f, "HTTP {status}: {lead}"),
            (None, false) => write!(f, "HTTP {status}: {message}"),
            (None, true) => write!(f, "HTTP {status}"),
        }
    }
}

/// Reads a non-success `response` into a failure, classified with `brand`'s
/// error codes (`catalog::error_code_brand`).
pub async fn read_failure(brand: &str, response: reqwest::Response) -> ProviderFailure {
    let url = response.url().to_string();
    let status = response.status().as_u16();
    let retry_after = retry_after(response.headers());
    let body = response.text().await.unwrap_or_default();
    let failure = classify_http(brand, status, retry_after, &body);
    tracing::warn!(%url, brand, ?failure, "Provider returned an error");
    failure
}

/// `Retry-After` in seconds; the HTTP-date form is not read.
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let seconds = headers
        .get(RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some(Duration::from_secs(seconds))
}

pub fn classify_http(
    brand: &str,
    status: u16,
    retry_after: Option<Duration>,
    body: &str,
) -> ProviderFailure {
    let (provider_code, message) = parse_error_body(body);
    let kind = classify(
        brand,
        status,
        retry_after.is_some(),
        provider_code.as_deref(),
        &message,
    );
    ProviderFailure {
        kind,
        status,
        provider_code,
        message,
        retry_after,
    }
}

// Matched as substrings, so each phrase must not occur inside an unrelated
// word or setting: "balance" matches "load balancer", "budget" matches
// "thinking.budget_tokens", a bare "monthly" matches "monthly plan".
const BILLING_PHRASES: &[&str] = &["insufficient balance", "credit", "billing", "payment"];

const QUOTA_PHRASES: &[&str] = &[
    "quota",
    "usage limit",
    "spend limit",
    "spending limit",
    "weekly limit",
    "monthly limit",
];

/// First match wins.
fn classify(
    brand: &str,
    status: u16,
    has_retry_after: bool,
    code: Option<&str>,
    message: &str,
) -> FailureKind {
    // 1. A 429 that says when to come back is throttling, whatever its text.
    if status == 429 && has_retry_after {
        return FailureKind::Transient;
    }
    // 2. The brand's own code.
    if let Some(kind) = code.and_then(|code| code_kind(brand, code)) {
        return kind;
    }
    // 3. Statuses with a single meaning. 529 is Anthropic's overload.
    match status {
        401 => return FailureKind::Auth,
        402 => return FailureKind::Billing,
        408 | 500..=599 => return FailureKind::Transient,
        413 => return FailureKind::ContextTooLong,
        _ => {}
    }
    // 4. Brands also report spend and plan limits as 400, 403 or 429, so the
    //    text decides. Neither list holds "rate limit": that is throttling.
    if (400..500).contains(&status) {
        let lower = message.to_ascii_lowercase();
        let mentions = |phrases: &[&str]| phrases.iter().any(|phrase| lower.contains(phrase));
        if mentions(BILLING_PHRASES) {
            return FailureKind::Billing;
        }
        if mentions(QUOTA_PHRASES) {
            return FailureKind::UsageLimit;
        }
    }
    // 5. What is left.
    match status {
        429 => FailureKind::Transient,
        403 => FailureKind::Auth,
        _ if is_context_limit_error(message) => FailureKind::ContextTooLong,
        _ => FailureKind::Other,
    }
}

fn code_kind(brand: &str, code: &str) -> Option<FailureKind> {
    error_codes(brand)
        .iter()
        .find(|(known, _)| *known == code)
        .map(|(_, kind)| *kind)
}

/// Codes whose meaning the status gets wrong, per brand.
fn error_codes(brand: &str) -> &'static [(&'static str, FailureKind)] {
    match brand {
        "zai" => ZAI_ERROR_CODES,
        _ => &[],
    }
}

/// From https://docs.z.ai/api-reference/api-code. All but the 400s are sent
/// as HTTP 429.
const ZAI_ERROR_CODES: &[(&str, FailureKind)] = &[
    ("1113", FailureKind::Billing),          // insufficient balance
    ("1211", FailureKind::ModelUnavailable), // unknown model (400)
    ("1261", FailureKind::ContextTooLong),   // prompt too long (400)
    ("1301", FailureKind::ContentFiltered),  // sensitive content (400)
    ("1302", FailureKind::Transient),        // rate limit
    ("1305", FailureKind::Transient),        // overloaded
    ("1308", FailureKind::UsageLimit),       // usage limit, resets at a stated time
    ("1309", FailureKind::Billing),          // coding plan expired
    ("1310", FailureKind::UsageLimit),       // weekly or monthly limit
    ("1311", FailureKind::ModelUnavailable), // plan does not include the model
    ("1313", FailureKind::Other),            // fair-use restriction: ask support
    ("1314", FailureKind::Billing),          // enterprise package expired
    ("1315", FailureKind::Auth),             // key is for another product type
    ("1316", FailureKind::UsageLimit),       // 5-hour limit, no balance for extra usage
    ("1317", FailureKind::UsageLimit),       // 7-day limit, no balance for extra usage
    ("1318", FailureKind::UsageLimit),       // 5-hour limit, monthly spend limit
    ("1319", FailureKind::UsageLimit),       // 7-day limit, monthly spend limit
    ("1320", FailureKind::UsageLimit),       // 5-hour limit, monthly spend limit
    ("1321", FailureKind::UsageLimit),       // 7-day limit, monthly spend limit
];

/// `error.code` and `error.message` from the OpenAI and Anthropic envelopes.
/// A string `error` or a top-level `message` also serves as the message; any
/// other body is its own message.
fn parse_error_body(body: &str) -> (Option<String>, String) {
    let body = body.trim();
    let Ok(json) = serde_json::from_str::<Value>(body) else {
        return (None, body.to_string());
    };
    let error = &json["error"];
    let code = match &error["code"] {
        Value::String(code) => Some(code.clone()),
        Value::Number(code) => Some(code.to_string()),
        _ => None,
    };
    let message = error["message"]
        .as_str()
        .or_else(|| error.as_str())
        .or_else(|| json["message"].as_str())
        .unwrap_or(body);
    (code, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::local_agent::is_usage_limit_error;
    use FailureKind::*;

    fn zai(code: &str, message: &str) -> String {
        serde_json::json!({ "error": { "code": code, "message": message } }).to_string()
    }

    fn anthropic(error_type: &str, message: &str) -> String {
        serde_json::json!({
            "type": "error",
            "error": { "type": error_type, "message": message },
        })
        .to_string()
    }

    fn openai(code: Value, message: &str) -> String {
        serde_json::json!({ "error": { "code": code, "message": message } }).to_string()
    }

    #[test]
    fn classify_http_cases() {
        let wait = Some(Duration::from_secs(7));
        #[rustfmt::skip]
        let cases = vec![
            ("zai", 429, None, zai("1302", "Rate limit reached for requests"), Transient, Some("1302")),
            ("zai", 429, None, zai("1305", "The service may be temporarily overloaded"), Transient, Some("1305")),
            ("zai", 429, None, zai("1308", "Usage limit reached for 5 hours. Your limit will reset at 2026-11-17 14:00"), UsageLimit, Some("1308")),
            ("zai", 429, None, zai("1310", "Weekly/Monthly Limit Exhausted. Your limit will reset at 2026-11-20"), UsageLimit, Some("1310")),
            ("zai", 429, None, zai("1113", "Insufficient balance or no resource package. Please recharge."), Billing, Some("1113")),
            ("zai", 429, None, zai("1309", "Your GLM Coding Plan package has expired"), Billing, Some("1309")),
            ("zai", 400, None, zai("1211", "Unknown Model, please check the model code."), ModelUnavailable, Some("1211")),
            ("zai", 429, None, zai("1311", "Your current subscription plan does not yet include access to glm-5"), ModelUnavailable, Some("1311")),
            ("zai", 400, None, zai("1261", "Prompt exceeds max length"), ContextTooLong, Some("1261")),
            ("zai", 400, None, zai("1301", "System detected potentially unsafe or sensitive content"), ContentFiltered, Some("1301")),
            ("zai", 502, None, "error code: 502".into(), Transient, None),
            ("anthropic", 529, None, anthropic("overloaded_error", "Overloaded"), Transient, None),
            ("anthropic", 429, wait, anthropic("rate_limit_error", "Number of request tokens has exceeded your per-minute rate limit"), Transient, None),
            ("anthropic", 429, None, anthropic("rate_limit_error", "You have reached your specified workspace spend limit"), UsageLimit, None),
            ("anthropic", 400, None, anthropic("invalid_request_error", "You have reached your specified API usage limits. You will regain access on 2026-12-01"), UsageLimit, None),
            ("anthropic", 400, None, anthropic("invalid_request_error", "prompt is too long: 210000 tokens > 200000 maximum"), ContextTooLong, None),
            ("anthropic", 402, None, anthropic("billing_error", "Your account has a billing issue"), Billing, None),
            ("anthropic", 400, None, anthropic("invalid_request_error", "Your credit balance is too low to access the Anthropic API"), Billing, None),
            ("anthropic", 401, None, anthropic("authentication_error", "invalid x-api-key"), Auth, None),
            ("openrouter", 402, None, openai(402.into(), "Insufficient credits"), Billing, Some("402")),
            ("openrouter", 429, wait, openai(429.into(), "Rate limit exceeded"), Transient, Some("429")),
            ("custom-openai", 429, wait, openai(Value::Null, "Per-minute token quota exceeded"), Transient, None),
            ("custom-openai", 403, None, openai(Value::Null, "Key limit exceeded (monthly limit)"), UsageLimit, None),
            ("openai", 429, None, openai("insufficient_quota".into(), "You exceeded your current quota, please check your plan and billing details."), Billing, Some("insufficient_quota")),
            ("custom-openai", 408, None, "Request Timeout".into(), Transient, None),
            ("custom-openai", 413, None, "Request Entity Too Large".into(), ContextTooLong, None),
            ("custom-openai", 403, None, openai(Value::Null, "Forbidden"), Auth, None),
            // Phrases that only look like a billing or quota limit.
            ("anthropic", 400, None, anthropic("invalid_request_error", "`max_tokens` must be greater than `thinking.budget_tokens`"), Other, None),
            ("custom-openai", 421, None, "Misdirected request at the load balancer".into(), Other, None),
            ("custom-openai", 400, None, openai(Value::Null, "Your monthly plan does not support this parameter"), Other, None),
            ("deepseek", 402, None, openai(Value::Null, "Insufficient Balance"), Billing, None),
            ("deepseek", 503, None, openai(Value::Null, "Server overloaded"), Transient, None),
            ("custom-openai", 500, None, "upstream exploded".into(), Transient, None),
            ("custom-openai", 429, None, openai(Value::Null, "Rate limit exceeded"), Transient, None),
            ("custom-openai", 429, None, openai("rate_limit".into(), "weekly quota reached"), UsageLimit, Some("rate_limit")),
            ("custom-openai", 400, None, openai("context_length_exceeded".into(), "This model's maximum context length is 8192 tokens"), ContextTooLong, Some("context_length_exceeded")),
            ("custom-openai", 404, None, openai("model_not_found".into(), "The model `x` does not exist"), Other, Some("model_not_found")),
            // Z.ai's codes apply to the Z.ai brand only.
            ("custom-openai", 429, None, zai("1305", "The service may be temporarily overloaded"), Transient, Some("1305")),
            ("custom-openai", 429, None, zai("1309", "Your GLM Coding Plan package has expired"), Transient, Some("1309")),
        ];
        for (brand, status, retry_after, body, kind, code) in cases {
            let failure = classify_http(brand, status, retry_after, &body);
            let row = format!("{brand} {status} {body}");
            assert_eq!(failure.kind, kind, "{row}");
            assert_eq!(failure.provider_code.as_deref(), code, "{row}");
            assert_eq!(failure.retry_after, retry_after, "{row}");
            assert_eq!(failure.status, status, "{row}");
        }
    }

    #[test]
    fn the_provider_message_is_kept_after_the_lead() {
        let failure = classify_http(
            "zai",
            429,
            None,
            &zai("1113", "Insufficient balance, top up at https://z.ai"),
        );
        assert_eq!(
            failure.to_string(),
            "HTTP 429: The provider account needs a top-up or a plan renewal. \
             Insufficient balance, top up at https://z.ai"
        );
        let unparsed = classify_http("custom-openai", 418, None, "  I'm a teapot\n");
        assert_eq!(unparsed.to_string(), "HTTP 418: I'm a teapot");
        let empty = classify_http("custom-openai", 418, None, "");
        assert_eq!(empty.to_string(), "HTTP 418");
    }

    #[test]
    fn plain_string_and_top_level_messages_are_read() {
        let (_, message) = parse_error_body(r#"{"error":"model 'x' not found"}"#);
        assert_eq!(message, "model 'x' not found");
        let (_, message) = parse_error_body(r#"{"message":"Forbidden"}"#);
        assert_eq!(message, "Forbidden");
    }

    #[test]
    fn retry_after_reads_seconds_only() {
        let mut headers = HeaderMap::new();
        assert_eq!(retry_after(&headers), None);
        headers.insert(RETRY_AFTER, " 12 ".parse().unwrap());
        assert_eq!(retry_after(&headers), Some(Duration::from_secs(12)));
        headers.insert(
            RETRY_AFTER,
            "Wed, 21 Oct 2026 07:28:00 GMT".parse().unwrap(),
        );
        assert_eq!(retry_after(&headers), None);
    }

    #[test]
    fn error_codes_have_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for (code, _) in ZAI_ERROR_CODES {
            assert!(seen.insert(code), "Z.ai lists {code} twice");
        }
    }

    /// Transitional, until the frontend reads `error_kind` (PR 3):
    /// `runErrors.ts` marks a run error as a limit by phrase, so only the
    /// `UsageLimit` lead may contain one.
    #[test]
    fn only_the_usage_limit_lead_names_a_limit() {
        for kind in FailureKind::ALL {
            let lead = kind.lead().unwrap_or_default();
            assert_eq!(
                is_usage_limit_error(lead),
                kind == UsageLimit,
                "{kind:?}: {lead}"
            );
        }
    }
}
