use keyring::Entry;

const SERVICE_NAME: &str = "com.juacker.clai.providers";
const MCP_SERVICE_NAME: &str = "com.juacker.clai.mcp";

fn entry_for(service_name: &str, secret_ref: &str) -> Result<Entry, keyring::Error> {
    Entry::new(service_name, secret_ref)
}

/// Store a secret, transparently creating the Secret Service default
/// collection when it is missing (Linux only).
///
/// On live sessions, autologin setups, and fresh accounts, gnome-keyring may
/// be running with *no* keyring collection at all: PAM only creates and
/// unlocks the "login" collection during a password login. The Secret
/// Service then reports "no result found" for the `default` alias, and the
/// `keyring` crate cannot self-heal because its create path for the special
/// `default` target only re-reads the alias instead of creating a
/// collection. In that case we ask the Secret Service daemon to create a
/// collection with the `default` alias — the daemon shows its own password
/// dialog, so the user stays in control of the keyring password — and then
/// retry the write once.
fn set_password_creating_default_collection(
    entry: &Entry,
    secret: &str,
) -> Result<(), keyring::Error> {
    match entry.set_password(secret) {
        #[cfg(target_os = "linux")]
        Err(error) if is_missing_default_collection(&error) => {
            create_default_collection()?;
            entry.set_password(secret)
        }
        result => result,
    }
}

/// True when the keyring error means the Secret Service is reachable but has
/// no collection behind the `default` alias ("no result found").
#[cfg(target_os = "linux")]
fn is_missing_default_collection(error: &keyring::Error) -> bool {
    matches!(error, keyring::Error::NoStorageAccess(_))
        && error
            .to_string()
            .to_ascii_lowercase()
            .contains("no result found")
}

/// Create a Secret Service collection registered as the `default` alias.
///
/// The Secret Service daemon (gnome-keyring, KWallet, ...) owns the
/// creation: it prompts the user for the new keyring's password through its
/// own dialog, exactly as `secret-tool` or Seahorse would. Every libsecret
/// consumer resolves the same `default` alias afterwards, so the collection
/// created here is shared system-wide, not CLAI-specific.
#[cfg(target_os = "linux")]
fn create_default_collection() -> Result<(), keyring::Error> {
    use dbus_secret_service::{EncryptionType, Error as SsError, SecretService};

    let session = SecretService::connect(EncryptionType::Plain)
        .map_err(|error| keyring::Error::PlatformFailure(Box::new(error)))?;
    match session.create_collection("Default keyring", "default") {
        Ok(_) => Ok(()),
        // The user dismissed the daemon's "create keyring" password dialog.
        Err(SsError::Prompt) => Err(keyring::Error::NoStorageAccess(
            "no default keyring exists and the keyring creation dialog was dismissed. \
             Retry and choose a password, or create a default keyring with Seahorse \
             (\"Passwords and Keys\"). On autologin setups, enabling password login \
             lets the system create and unlock the keyring automatically."
                .into(),
        )),
        Err(error) => Err(keyring::Error::PlatformFailure(Box::new(error))),
    }
}

pub struct ProviderSecretStorage;

impl ProviderSecretStorage {
    fn entry(secret_ref: &str) -> Result<Entry, keyring::Error> {
        entry_for(SERVICE_NAME, secret_ref)
    }

    pub fn set_secret(secret_ref: &str, secret: &str) -> Result<(), keyring::Error> {
        let entry = Self::entry(secret_ref)?;
        set_password_creating_default_collection(&entry, secret)
    }

    #[allow(dead_code)]
    pub fn get_secret(secret_ref: &str) -> Result<Option<String>, keyring::Error> {
        let entry = Self::entry(secret_ref)?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn clear_secret(secret_ref: &str) -> Result<(), keyring::Error> {
        let entry = Self::entry(secret_ref)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error),
        }
    }
}

pub struct McpSecretStorage;

impl McpSecretStorage {
    fn entry(secret_ref: &str) -> Result<Entry, keyring::Error> {
        entry_for(MCP_SERVICE_NAME, secret_ref)
    }

    pub fn set_secret(secret_ref: &str, secret: &str) -> Result<(), keyring::Error> {
        let entry = Self::entry(secret_ref)?;
        set_password_creating_default_collection(&entry, secret)
    }

    pub fn get_secret(secret_ref: &str) -> Result<Option<String>, keyring::Error> {
        let entry = Self::entry(secret_ref)?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn clear_secret(secret_ref: &str) -> Result<(), keyring::Error> {
        let entry = Self::entry(secret_ref)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error),
        }
    }
}
