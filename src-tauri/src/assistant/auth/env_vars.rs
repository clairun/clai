//! User-configured environment variables (or HTTP headers) whose secret
//! values live in the OS vault, never in config. Shared by MCP servers and
//! CLI provider connections; each owner names where its secrets live with an
//! [`EnvSecrets`].

use std::collections::HashSet;

use super::Vault;
use crate::config::McpEnvVar;

/// Rejects what a process environment cannot hold: a name that is empty,
/// contains `=` or NUL, or is given twice, and a value containing NUL.
pub fn validate_env_vars(vars: &[McpEnvVar]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for var in vars {
        if var.key.is_empty() {
            return Err("Environment variable name cannot be empty".to_string());
        }
        if var.key.contains(['=', '\0']) {
            return Err(format!(
                "Environment variable name `{}` cannot contain '=' or NUL",
                var.key.escape_debug()
            ));
        }
        if !seen.insert(var.key.as_str()) {
            return Err(format!(
                "Environment variable `{}` is set more than once",
                var.key
            ));
        }
        if var
            .value
            .as_deref()
            .is_some_and(|value| value.contains('\0'))
        {
            return Err(format!(
                "Environment variable `{}` cannot contain a NUL character",
                var.key
            ));
        }
    }
    Ok(())
}

/// The value to use for `var`: its plain value, or its secret read from the
/// vault now, at spawn/connect time. A secret that is missing or unreadable
/// is an error that names only the key.
pub fn resolve_env_value(vault: Vault, var: &McpEnvVar) -> Result<Option<String>, String> {
    if !var.secret {
        return Ok(var.value.clone());
    }
    let stored = match var.secret_ref.as_deref() {
        Some(secret_ref) => vault
            .get(secret_ref)
            .map_err(|error| format!("Cannot read the secret for `{}`: {error}", var.key))?,
        None => None,
    };
    match stored {
        Some(value) => Ok(Some(value)),
        None => Err(format!("No secret is stored for `{}`", var.key)),
    }
}

/// Where one owner's env var (or header) secrets live: the vault service and
/// the prefix of every entry's reference.
pub struct EnvSecrets {
    vault: Vault,
    ref_prefix: String,
}

impl EnvSecrets {
    pub fn new(vault: Vault, ref_prefix: String) -> Self {
        Self { vault, ref_prefix }
    }

    /// Stores each secret value in the vault under `{ref_prefix}::{key}` and
    /// returns the entries to persist: secret entries carry that `secret_ref`
    /// and no value. A secret sent without a value keeps the one stored for
    /// the same key in `previous`, so the user need not re-enter it; with
    /// nothing stored under that key (a new or renamed secret) it is an error.
    pub fn store(
        &self,
        incoming: Vec<McpEnvVar>,
        previous: &[McpEnvVar],
    ) -> Result<Vec<McpEnvVar>, String> {
        let mut out = Vec::with_capacity(incoming.len());
        for var in incoming {
            if !var.secret {
                out.push(McpEnvVar {
                    secret_ref: None,
                    ..var
                });
                continue;
            }
            let secret_ref = match var.value.as_deref() {
                Some(value) if !value.is_empty() => {
                    let secret_ref = format!("{}::{}", self.ref_prefix, var.key);
                    self.vault
                        .set(&secret_ref, value)
                        .map_err(|e| format!("Failed to store secret `{}`: {}", var.key, e))?;
                    secret_ref
                }
                _ => previous
                    .iter()
                    .filter(|p| p.secret && p.key == var.key)
                    .find_map(|p| p.secret_ref.clone())
                    .ok_or_else(|| format!("Enter a value for the secret `{}`", var.key))?,
            };
            out.push(McpEnvVar {
                key: var.key,
                value: None,
                secret_ref: Some(secret_ref),
                secret: true,
            });
        }
        Ok(out)
    }

    /// Removes from the vault the secrets `previous` holds that `next` no
    /// longer references (removed, renamed, or made non-secret).
    pub fn clear_orphaned(&self, previous: &[McpEnvVar], next: &[McpEnvVar]) {
        let keep: HashSet<&str> = secret_refs(next).collect();
        for secret_ref in secret_refs(previous).filter(|r| !keep.contains(r)) {
            self.clear_secret(secret_ref);
        }
    }

    /// Removes every secret `vars` references from the vault.
    pub fn clear(&self, vars: &[McpEnvVar]) {
        secret_refs(vars).for_each(|secret_ref| self.clear_secret(secret_ref));
    }

    fn clear_secret(&self, secret_ref: &str) {
        if let Err(error) = self.vault.clear(secret_ref) {
            tracing::warn!(%error, secret_ref, "Failed to remove an env var secret from the vault");
        }
    }
}

fn secret_refs(vars: &[McpEnvVar]) -> impl Iterator<Item = &str> {
    vars.iter()
        .filter(|var| var.secret)
        .filter_map(|var| var.secret_ref.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assistant::auth::ProviderSecretStorage;

    fn plain(key: &str, value: &str) -> McpEnvVar {
        McpEnvVar {
            key: key.to_string(),
            value: Some(value.to_string()),
            secret_ref: None,
            secret: false,
        }
    }

    fn secret(key: &str, value: Option<&str>, secret_ref: Option<&str>) -> McpEnvVar {
        McpEnvVar {
            key: key.to_string(),
            value: value.map(str::to_string),
            secret_ref: secret_ref.map(str::to_string),
            secret: true,
        }
    }

    #[test]
    fn validate_env_vars_rejects_what_an_environment_cannot_hold() {
        assert!(validate_env_vars(&[plain("CLAUDE_CONFIG_DIR", "x"), plain("A", "")]).is_ok());
        for (vars, reason) in [
            (vec![plain("", "x")], "empty"),
            (vec![plain("A=B", "x")], "'='"),
            (vec![plain("A\0B", "x")], "NUL"),
            (vec![plain("A", "x"), plain("A", "y")], "more than once"),
            (vec![plain("A", "x\0y")], "`A` cannot contain a NUL"),
            (
                vec![secret("A", Some("x\0y"), None)],
                "`A` cannot contain a NUL",
            ),
        ] {
            let error = validate_env_vars(&vars).expect_err(reason);
            assert!(error.contains(reason), "{reason}: {error}");
        }
    }

    /// A secret that cannot be read is an error naming the key, so a CLI is
    /// never started without the variable that selects its account.
    #[test]
    fn resolve_env_value_fails_for_secrets_it_cannot_read() {
        let secret_ref = "provider-connection::resolve-env-test::env::TOKEN";
        ProviderSecretStorage::set_secret(secret_ref, "vault-value").unwrap();
        let resolve = |var: &McpEnvVar| resolve_env_value(Vault::PROVIDERS, var);
        assert_eq!(
            resolve(&secret("TOKEN", None, Some(secret_ref))),
            Ok(Some("vault-value".to_string()))
        );
        assert_eq!(resolve(&plain("PLAIN", "x")), Ok(Some("x".to_string())));

        ProviderSecretStorage::clear_secret(secret_ref).unwrap();
        for var in [
            secret("TOKEN", None, Some(secret_ref)),
            secret("TOKEN", None, None),
        ] {
            let error = resolve(&var).unwrap_err();
            assert!(error.contains("`TOKEN`"), "{error}");
            assert!(!error.contains("vault-value"), "{error}");
        }
    }

    #[test]
    fn store_moves_values_to_the_vault_and_keeps_unchanged_ones() {
        let prefix = "provider-connection::store-env-test::env";
        let secrets = EnvSecrets::new(Vault::PROVIDERS, prefix.to_string());
        let stored = secrets
            .store(
                vec![
                    plain("PLAIN", "inline"),
                    secret("TOKEN", Some("first"), None),
                ],
                &[],
            )
            .unwrap();
        let token_ref = format!("{prefix}::TOKEN");
        assert_eq!(
            stored,
            vec![
                plain("PLAIN", "inline"),
                secret("TOKEN", None, Some(&token_ref))
            ]
        );
        assert_eq!(
            ProviderSecretStorage::get_secret(&token_ref)
                .unwrap()
                .as_deref(),
            Some("first")
        );

        // Sent again without a value: the stored secret stays. A reference
        // sent by the client is ignored, so it cannot point at another
        // vault entry.
        let next = secrets
            .store(
                vec![secret("TOKEN", None, Some("provider-connection::other"))],
                &stored,
            )
            .unwrap();
        assert_eq!(next, vec![secret("TOKEN", None, Some(&token_ref))]);
        assert_eq!(
            ProviderSecretStorage::get_secret(&token_ref)
                .unwrap()
                .as_deref(),
            Some("first")
        );

        // A renamed secret has nothing stored under its new key.
        let error = secrets
            .store(vec![secret("RENAMED", None, None)], &stored)
            .unwrap_err();
        assert!(error.contains("`RENAMED`"), "{error}");

        secrets.clear_orphaned(&stored, &[plain("TOKEN", "now plain")]);
        assert_eq!(ProviderSecretStorage::get_secret(&token_ref).unwrap(), None);
    }

    #[test]
    fn clear_removes_only_secret_entries() {
        let secret_ref = "provider-connection::clear-env-test::env::TOKEN";
        let unrelated = "provider-connection::clear-env-test";
        ProviderSecretStorage::set_secret(secret_ref, "value").unwrap();
        ProviderSecretStorage::set_secret(unrelated, "api-key").unwrap();
        // A non-secret entry's stray reference is not the owner's to delete.
        let mut stray = plain("PLAIN", "x");
        stray.secret_ref = Some(unrelated.to_string());

        EnvSecrets::new(
            Vault::PROVIDERS,
            "provider-connection::clear-env-test::env".into(),
        )
        .clear(&[stray, secret("TOKEN", None, Some(secret_ref))]);

        assert_eq!(ProviderSecretStorage::get_secret(secret_ref).unwrap(), None);
        assert_eq!(
            ProviderSecretStorage::get_secret(unrelated)
                .unwrap()
                .as_deref(),
            Some("api-key")
        );
        ProviderSecretStorage::clear_secret(unrelated).unwrap();
    }
}
