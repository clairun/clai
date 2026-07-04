use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use axum::{
    extract::{Query, State},
    response::Html,
    routing::get,
    Router,
};
use rmcp::transport::auth::{
    AuthError, AuthorizationManager, AuthorizationSession, CredentialStore, OAuthClientConfig,
    StoredCredentials,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Mutex as AsyncMutex};
use tokio_util::sync::CancellationToken;

use crate::assistant::auth::McpSecretStorage;
use crate::config::{McpServerAuth, McpServerConfig, McpServerTransport};

/// CLAI's OAuth client-ID metadata document (CIMD): the `client_id` IS this
/// URL, so changing it changes the OAuth client identity. It only applies to
/// NEWLY added MCP servers: the resolved URL is persisted per server at add
/// time, so existing connections keep refreshing against the URL stored in
/// their config (the previous `juacker.github.io/clai/auth/...` document
/// stays published for exactly that reason).
pub const CLAI_CLIENT_METADATA_URL: &str = "https://clai.run/auth/client-metadata.json";
pub const CLAI_OAUTH_CLIENT_NAME: &str = "CLAI";
pub const OAUTH_CALLBACK_PATH: &str = "/oauth/mcp/callback";
pub const OAUTH_LOGIN_TIMEOUT_SECS: u64 = 600;

const RUNTIME_REDIRECT_URI_PLACEHOLDER: &str = "http://127.0.0.1/oauth/mcp/callback";

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "bindings.ts")]
pub struct McpCatalogEntry {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub category: String,
    pub endpoint_url: String,
    pub auth_mode: String,
    pub logo_asset: String,
    pub suggested_scopes: Vec<String>,
    pub notes: Option<String>,
}

pub fn catalog_entries() -> Vec<McpCatalogEntry> {
    vec![
        McpCatalogEntry {
            id: "linear".to_string(),
            display_name: "Linear".to_string(),
            description: "Issues, projects, and product planning.".to_string(),
            category: "Issues / project management".to_string(),
            endpoint_url: "https://mcp.linear.app/mcp".to_string(),
            auth_mode: "oauth".to_string(),
            logo_asset: "mcp-catalog/linear.svg".to_string(),
            suggested_scopes: Vec::new(),
            notes: Some("Use the Streamable HTTP /mcp endpoint, not legacy /sse.".to_string()),
        },
        McpCatalogEntry {
            id: "sentry".to_string(),
            display_name: "Sentry".to_string(),
            description: "Errors, issues, releases, and observability context.".to_string(),
            category: "Errors / observability".to_string(),
            endpoint_url: "https://mcp.sentry.dev/mcp".to_string(),
            auth_mode: "oauth".to_string(),
            logo_asset: "mcp-catalog/sentry.svg".to_string(),
            suggested_scopes: Vec::new(),
            notes: Some("Official hosted server optimized for debugging workflows.".to_string()),
        },
        McpCatalogEntry {
            id: "notion".to_string(),
            display_name: "Notion".to_string(),
            description: "Workspace docs, pages, and knowledge base search.".to_string(),
            category: "Docs / knowledge base".to_string(),
            endpoint_url: "https://mcp.notion.com/mcp".to_string(),
            auth_mode: "oauth".to_string(),
            logo_asset: "mcp-catalog/notion.svg".to_string(),
            suggested_scopes: Vec::new(),
            notes: Some("Use the Streamable HTTP /mcp endpoint, not legacy /sse.".to_string()),
        },
        McpCatalogEntry {
            id: "miro".to_string(),
            display_name: "Miro".to_string(),
            description: "Whiteboards, boards, diagrams, and collaborative planning.".to_string(),
            category: "Whiteboard / collaboration".to_string(),
            endpoint_url: "https://mcp.miro.com".to_string(),
            auth_mode: "oauth".to_string(),
            logo_asset: "mcp-catalog/miro.svg".to_string(),
            suggested_scopes: Vec::new(),
            notes: Some(
                "Official hosted MCP server; enterprise workspaces may require admin enablement."
                    .to_string(),
            ),
        },
        McpCatalogEntry {
            id: "lucid".to_string(),
            display_name: "Lucid".to_string(),
            description: "Diagrams, whiteboards, docs, and visual collaboration.".to_string(),
            category: "Diagramming / collaboration".to_string(),
            endpoint_url: "https://mcp.lucid.app/mcp".to_string(),
            auth_mode: "oauth".to_string(),
            logo_asset: "mcp-catalog/lucid.svg".to_string(),
            suggested_scopes: Vec::new(),
            notes: Some("Official remote Streamable HTTP MCP server with OAuth.".to_string()),
        },
        McpCatalogEntry {
            id: "whimsical".to_string(),
            display_name: "Whimsical".to_string(),
            description: "Boards, docs, wireframes, flowcharts, and visual workspaces.".to_string(),
            category: "Visual workspace".to_string(),
            endpoint_url: "https://mcp.whimsical.com/mcp".to_string(),
            auth_mode: "oauth".to_string(),
            logo_asset: "mcp-catalog/whimsical.svg".to_string(),
            suggested_scopes: Vec::new(),
            notes: Some("Official remote MCP server for Whimsical workspaces.".to_string()),
        },
        McpCatalogEntry {
            id: "stripe".to_string(),
            display_name: "Stripe".to_string(),
            description: "Payments, customers, subscriptions, and account data.".to_string(),
            category: "Payments".to_string(),
            endpoint_url: "https://mcp.stripe.com".to_string(),
            auth_mode: "oauth".to_string(),
            logo_asset: "mcp-catalog/stripe.svg".to_string(),
            suggested_scopes: Vec::new(),
            notes: Some(
                "OAuth is preferred; API keys remain available through the custom path."
                    .to_string(),
            ),
        },
        McpCatalogEntry {
            id: "neon".to_string(),
            display_name: "Neon".to_string(),
            description: "Serverless Postgres projects, branches, and database operations."
                .to_string(),
            category: "Serverless Postgres".to_string(),
            endpoint_url: "https://mcp.neon.tech/mcp".to_string(),
            auth_mode: "oauth".to_string(),
            logo_asset: "mcp-catalog/neon.svg".to_string(),
            suggested_scopes: Vec::new(),
            notes: Some(
                "OAuth operates on the personal account unless an org/project id is supplied."
                    .to_string(),
            ),
        },
        McpCatalogEntry {
            id: "buildkite".to_string(),
            display_name: "Buildkite".to_string(),
            description: "CI pipelines, builds, jobs, annotations, and artifacts.".to_string(),
            category: "CI/CD".to_string(),
            endpoint_url: "https://mcp.buildkite.com/mcp".to_string(),
            auth_mode: "oauth".to_string(),
            logo_asset: "mcp-catalog/buildkite.svg".to_string(),
            suggested_scopes: Vec::new(),
            notes: Some("Buildkite pre-sets short-lived read-only OAuth scopes.".to_string()),
        },
        McpCatalogEntry {
            id: "netlify".to_string(),
            display_name: "Netlify".to_string(),
            description: "Sites, deploys, builds, forms, and hosting configuration.".to_string(),
            category: "Deploy / hosting".to_string(),
            endpoint_url: "https://netlify-mcp.netlify.app/mcp".to_string(),
            auth_mode: "oauth".to_string(),
            logo_asset: "mcp-catalog/netlify.svg".to_string(),
            suggested_scopes: Vec::new(),
            notes: Some("Official hosted server for Netlify site management.".to_string()),
        },
    ]
}

pub fn oauth_credential_ref(server_id: &str) -> String {
    format!("mcp-server::{}::oauth-credentials", server_id)
}

pub fn oauth_client_secret_ref(server_id: &str) -> String {
    format!("mcp-server::{}::oauth-client-secret", server_id)
}

#[derive(Debug, Clone)]
pub struct McpOAuthCredentialStore {
    credential_ref: String,
}

impl McpOAuthCredentialStore {
    pub fn new(credential_ref: impl Into<String>) -> Self {
        Self {
            credential_ref: credential_ref.into(),
        }
    }

    pub fn load_sync(&self) -> Result<Option<StoredCredentials>, AuthError> {
        let Some(raw) = McpSecretStorage::get_secret(&self.credential_ref)
            .map_err(|error| AuthError::InternalError(error.to_string()))?
        else {
            return Ok(None);
        };

        serde_json::from_str::<StoredCredentials>(&raw)
            .map(Some)
            .map_err(|error| {
                AuthError::InternalError(format!("Invalid stored OAuth credentials: {error}"))
            })
    }

    fn save_sync(&self, credentials: &StoredCredentials) -> Result<(), AuthError> {
        let raw = serde_json::to_string(credentials).map_err(|error| {
            AuthError::InternalError(format!("Failed to serialize OAuth credentials: {error}"))
        })?;
        McpSecretStorage::set_secret(&self.credential_ref, &raw)
            .map_err(|error| AuthError::InternalError(error.to_string()))
    }

    fn clear_sync(&self) -> Result<(), AuthError> {
        McpSecretStorage::clear_secret(&self.credential_ref)
            .map_err(|error| AuthError::InternalError(error.to_string()))
    }
}

#[async_trait]
impl CredentialStore for McpOAuthCredentialStore {
    async fn load(&self) -> Result<Option<StoredCredentials>, AuthError> {
        self.load_sync()
    }

    async fn save(&self, credentials: StoredCredentials) -> Result<(), AuthError> {
        self.save_sync(&credentials)
    }

    async fn clear(&self) -> Result<(), AuthError> {
        self.clear_sync()
    }
}

pub fn has_stored_oauth_credentials(credential_ref: &str) -> bool {
    McpOAuthCredentialStore::new(credential_ref)
        .load_sync()
        .ok()
        .flatten()
        .and_then(|credentials| credentials.token_response)
        .is_some()
}

pub fn has_stored_oauth_client_secret(server_id: &str) -> bool {
    McpSecretStorage::get_secret(&oauth_client_secret_ref(server_id))
        .ok()
        .flatten()
        .map(|secret| !secret.trim().is_empty())
        .unwrap_or(false)
}

pub fn clear_oauth_secrets(server_id: &str, credential_ref: &str) -> Result<(), String> {
    McpSecretStorage::clear_secret(credential_ref)
        .map_err(|error| format!("Failed to clear MCP OAuth credentials: {error}"))?;
    McpSecretStorage::clear_secret(&oauth_client_secret_ref(server_id))
        .map_err(|error| format!("Failed to clear MCP OAuth client secret: {error}"))?;
    Ok(())
}

pub async fn runtime_auth_manager_for_server(
    config: &McpServerConfig,
) -> Result<AuthorizationManager, String> {
    let McpServerAuth::OAuth {
        credential_ref,
        client_id,
        scopes,
        ..
    } = &config.auth
    else {
        return Err("MCP server is not configured for OAuth".to_string());
    };

    let McpServerTransport::Http { url } = &config.transport else {
        return Err("OAuth is only supported for HTTP MCP servers".to_string());
    };

    let store = McpOAuthCredentialStore::new(credential_ref.clone());
    let stored = store
        .load()
        .await
        .map_err(|error| format!("Failed to read OAuth credentials: {error}"))?;
    let configured_client_id = client_id
        .clone()
        .or_else(|| {
            stored
                .as_ref()
                .map(|credentials| credentials.client_id.clone())
        })
        .ok_or_else(|| "OAuth credentials are missing; reconnect this MCP server".to_string())?;

    let mut manager = AuthorizationManager::new(url.clone())
        .await
        .map_err(|error| format!("Failed to initialize OAuth manager: {error}"))?;
    manager.set_credential_store(store);
    let metadata = manager
        .discover_metadata()
        .await
        .map_err(|error| format!("OAuth discovery failed: {error}"))?;
    manager.set_metadata(metadata);

    let mut client_config =
        OAuthClientConfig::new(configured_client_id, RUNTIME_REDIRECT_URI_PLACEHOLDER)
            .with_scopes(scopes.clone());
    if let Some(secret) = McpSecretStorage::get_secret(&oauth_client_secret_ref(&config.id))
        .map_err(|error| format!("Failed to read OAuth client secret: {error}"))?
        .filter(|secret| !secret.trim().is_empty())
    {
        client_config = client_config.with_client_secret(secret);
    }
    manager
        .configure_client(client_config)
        .map_err(|error| format!("Failed to configure OAuth client: {error}"))?;
    Ok(manager)
}

pub fn callback_issuer_matches(
    expected_issuer: Option<&str>,
    callback_issuer: Option<&str>,
) -> bool {
    match (expected_issuer, callback_issuer) {
        (Some(expected), Some(actual)) => expected == actual,
        _ => true,
    }
}

#[derive(Debug, Clone)]
pub struct McpOAuthCallback {
    pub code: String,
    pub state: String,
    pub iss: Option<String>,
}

#[derive(Debug)]
pub struct McpOAuthCallbackListener {
    pub redirect_uri: String,
    pub receiver: oneshot::Receiver<Result<McpOAuthCallback, String>>,
    pub cancellation_token: CancellationToken,
}

type CallbackSender = Arc<AsyncMutex<Option<oneshot::Sender<Result<McpOAuthCallback, String>>>>>;

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    iss: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

pub async fn start_callback_listener() -> Result<McpOAuthCallbackListener, String> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| format!("Failed to bind OAuth callback listener: {error}"))?;
    let addr = listener
        .local_addr()
        .map_err(|error| format!("Failed to read OAuth callback listener address: {error}"))?;
    let redirect_uri = format!("http://{}{}", addr, OAUTH_CALLBACK_PATH);
    let (sender, receiver) = oneshot::channel();
    let sender = Arc::new(AsyncMutex::new(Some(sender)));
    let cancellation_token = CancellationToken::new();
    let router = Router::new()
        .route(OAUTH_CALLBACK_PATH, get(handle_oauth_callback))
        .with_state(sender);
    let shutdown = cancellation_token.clone();

    tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, router)
            .with_graceful_shutdown(async move { shutdown.cancelled_owned().await })
            .await
        {
            tracing::warn!(error = %error, "OAuth callback listener exited with error");
        }
    });

    Ok(McpOAuthCallbackListener {
        redirect_uri,
        receiver,
        cancellation_token,
    })
}

async fn handle_oauth_callback(
    Query(query): Query<CallbackQuery>,
    State(sender): State<CallbackSender>,
) -> Html<&'static str> {
    let result = callback_result(query);
    if let Some(sender) = sender.lock().await.take() {
        let _ = sender.send(result);
    }
    Html(
        r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>CLAI OAuth</title></head>
<body style="font-family: system-ui, sans-serif; margin: 2rem;">
  <h1>You can return to CLAI.</h1>
  <p>This browser tab no longer needs to stay open.</p>
</body>
</html>"#,
    )
}

fn callback_result(query: CallbackQuery) -> Result<McpOAuthCallback, String> {
    if let Some(error) = query.error {
        let description = query
            .error_description
            .filter(|value| !value.trim().is_empty())
            .map(|value| format!(": {value}"))
            .unwrap_or_default();
        return Err(format!("OAuth authorization failed: {error}{description}"));
    }

    let code = query
        .code
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "OAuth callback did not include an authorization code".to_string())?;
    let state = query
        .state
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "OAuth callback did not include state".to_string())?;

    Ok(McpOAuthCallback {
        code,
        state,
        iss: query.iss.filter(|value| !value.trim().is_empty()),
    })
}

#[derive(Default)]
pub struct McpOAuthSessionRegistry {
    sessions: HashMap<String, PendingMcpOAuthSession>,
}

impl McpOAuthSessionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, session: PendingMcpOAuthSession) {
        self.sessions.insert(session.login_id.clone(), session);
    }

    pub fn remove(&mut self, login_id: &str) -> Option<PendingMcpOAuthSession> {
        self.sessions.remove(login_id)
    }

    pub fn cancel(&mut self, login_id: &str) -> bool {
        if let Some(session) = self.sessions.remove(login_id) {
            session.callback_listener.cancellation_token.cancel();
            true
        } else {
            false
        }
    }
}

pub struct PendingMcpOAuthSession {
    pub login_id: String,
    pub draft: PendingMcpOAuthDraft,
    pub expected_issuer: Option<String>,
    pub authorization_session: AuthorizationSession,
    pub callback_listener: McpOAuthCallbackListener,
}

#[derive(Debug, Clone)]
pub struct PendingMcpOAuthDraft {
    pub existing_server_id: Option<String>,
    pub server_id: String,
    pub name: String,
    pub enabled: bool,
    pub url: String,
    pub scopes: Vec<String>,
    pub client_metadata_url: Option<String>,
    pub manual_client_secret: Option<String>,
}

pub async fn build_authorization_session(
    url: &str,
    credential_ref: String,
    scopes: &[String],
    redirect_uri: &str,
    client_id: Option<&str>,
    client_secret: Option<&str>,
    client_metadata_url: Option<&str>,
) -> Result<(AuthorizationSession, Option<String>), String> {
    let mut manager = AuthorizationManager::new(url.to_string())
        .await
        .map_err(|error| format!("Failed to initialize OAuth manager: {error}"))?;
    manager.set_credential_store(McpOAuthCredentialStore::new(credential_ref));
    let metadata = manager
        .discover_metadata()
        .await
        .map_err(|error| format!("OAuth discovery failed: {error}"))?;
    let expected_issuer = metadata.issuer.clone();
    manager.set_metadata(metadata);

    let scope_refs = scopes.iter().map(String::as_str).collect::<Vec<_>>();
    let session = if let Some(client_id) = client_id.filter(|value| !value.trim().is_empty()) {
        let mut config = OAuthClientConfig::new(client_id.to_string(), redirect_uri.to_string())
            .with_scopes(scopes.to_vec());
        if let Some(client_secret) = client_secret.filter(|value| !value.trim().is_empty()) {
            config = config.with_client_secret(client_secret.to_string());
        }
        manager
            .configure_client(config)
            .map_err(|error| format!("Failed to configure OAuth client: {error}"))?;
        let auth_url = manager
            .get_authorization_url(&scope_refs)
            .await
            .map_err(|error| format!("Failed to build OAuth authorization URL: {error}"))?;
        AuthorizationSession::for_scope_upgrade(manager, auth_url, redirect_uri)
    } else {
        AuthorizationSession::new(
            manager,
            &scope_refs,
            redirect_uri,
            Some(CLAI_OAUTH_CLIENT_NAME),
            client_metadata_url.or(Some(CLAI_CLIENT_METADATA_URL)),
        )
        .await
        .map_err(|error| format!("Failed to prepare OAuth authorization: {error}"))?
    };

    Ok((session, expected_issuer))
}
