//! Pluggable secret provider abstraction.
//!
//! Resolves secrets (Git tokens, SSH key paths, webhook HMAC secrets, ...) for
//! a given project. Implementations exist for environment variables and for a
//! mounted directory of files. A future Vault/SOPS adapter slots in behind the
//! same trait without touching call sites.

use std::env;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use domain::ProjectId;
use thiserror::Error;
use tokio::fs;
use tracing::{debug, warn};

pub mod webhook;

/// Strongly-typed key for a known secret. Stringly-typed escape hatch lives in
/// `SecretKey::Custom` for migration ergonomics — prefer adding variants over
/// abusing it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SecretKey {
    GitToken,
    SshKeyPath,
    SshKeyPassphrase,
    GitHttpToken,
    GitHttpUser,
    WebhookHmac,
    TriggerSecret,
    Custom(&'static str),
}

impl SecretKey {
    pub fn as_str(&self) -> &str {
        match self {
            SecretKey::GitToken => "git_token",
            SecretKey::SshKeyPath => "ssh_key_path",
            SecretKey::SshKeyPassphrase => "ssh_key_passphrase",
            SecretKey::GitHttpToken => "git_http_token",
            SecretKey::GitHttpUser => "git_http_user",
            SecretKey::WebhookHmac => "webhook_hmac",
            SecretKey::TriggerSecret => "trigger_secret",
            SecretKey::Custom(name) => name,
        }
    }

    /// Conventional environment variable name (uppercase, prefixed) when the
    /// secret is fetched without a project scope. `Custom(...)` callers are
    /// expected to pass an already-uppercased static identifier.
    pub fn env_var(&self) -> &'static str {
        match self {
            SecretKey::GitToken => "GIT_TOKEN",
            SecretKey::SshKeyPath => "SSH_KEY_PATH",
            SecretKey::SshKeyPassphrase => "SSH_KEY_PASSPHRASE",
            SecretKey::GitHttpToken => "GIT_HTTP_TOKEN",
            SecretKey::GitHttpUser => "GIT_HTTP_USER",
            SecretKey::WebhookHmac => "WEBHOOK_HMAC_SECRET",
            SecretKey::TriggerSecret => "TRIGGER_SECRET",
            SecretKey::Custom(name) => name,
        }
    }
}

#[derive(Debug, Error)]
pub enum SecretError {
    #[error("secret '{key}' not found for {scope}")]
    NotFound { key: String, scope: String },
    #[error("io error reading secret '{key}' from {path}: {source}")]
    Io {
        key: String,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid backend configuration: {0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, SecretError>;

/// Provider contract. Implementations must be cheap to clone (typically just
/// `Arc`-wrapped state) so call sites can pass them around.
#[async_trait]
pub trait SecretProvider: Send + Sync + std::fmt::Debug {
    /// Resolve a secret. `project` is `None` for global / legacy lookups.
    async fn get(&self, project: Option<ProjectId>, key: &SecretKey) -> Result<String>;

    /// Convenience: returns `None` instead of `NotFound`.
    async fn get_optional(
        &self,
        project: Option<ProjectId>,
        key: &SecretKey,
    ) -> Result<Option<String>> {
        match self.get(project, key).await {
            Ok(v) => Ok(Some(v)),
            Err(SecretError::NotFound { .. }) => Ok(None),
            Err(other) => Err(other),
        }
    }

    /// Human-readable backend name (for logs / health endpoints).
    fn backend_name(&self) -> &'static str;
}

/// Reads secrets from process environment variables. Project-scoped lookups
/// fall through to the unscoped variant (e.g. `GIT_TOKEN_<SLUG>` then
/// `GIT_TOKEN`) — the slug is unknown here, so callers that need per-project
/// env split should pass `Custom("GIT_TOKEN_FLUTTER_MONOREPO")` directly.
#[derive(Debug, Default, Clone)]
pub struct EnvSecretProvider;

impl EnvSecretProvider {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl SecretProvider for EnvSecretProvider {
    async fn get(&self, _project: Option<ProjectId>, key: &SecretKey) -> Result<String> {
        let name = key.env_var();
        match env::var(name) {
            Ok(v) if !v.is_empty() => Ok(v),
            _ => Err(SecretError::NotFound {
                key: key.as_str().to_owned(),
                scope: format!("env:{name}"),
            }),
        }
    }

    fn backend_name(&self) -> &'static str {
        "env"
    }
}

/// Reads secrets from `<root>/<project_id>/<key>` (or `<root>/_global/<key>`
/// when no project is supplied). One file per secret. Trailing whitespace is
/// trimmed. Mode 0600 recommended; this provider does not enforce file mode.
#[derive(Debug, Clone)]
pub struct FileSecretProvider {
    root: PathBuf,
}

impl FileSecretProvider {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn path_for(&self, project: Option<ProjectId>, key: &SecretKey) -> PathBuf {
        let scope = project
            .map(|p| p.as_uuid().to_string())
            .unwrap_or_else(|| "_global".to_owned());
        self.root.join(scope).join(key.as_str())
    }
}

#[async_trait]
impl SecretProvider for FileSecretProvider {
    async fn get(&self, project: Option<ProjectId>, key: &SecretKey) -> Result<String> {
        let path = self.path_for(project, key);
        debug!(target = "secrets", path = %path.display(), "reading file-backed secret");
        if !path.exists() {
            return Err(SecretError::NotFound {
                key: key.as_str().to_owned(),
                scope: format!("file:{}", path.display()),
            });
        }
        let bytes = fs::read(&path).await.map_err(|source| SecretError::Io {
            key: key.as_str().to_owned(),
            path: path.clone(),
            source,
        })?;
        let value = String::from_utf8(bytes).map_err(|err| SecretError::Io {
            key: key.as_str().to_owned(),
            path: path.clone(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, err),
        })?;
        Ok(value.trim_end_matches(['\n', '\r']).to_owned())
    }

    fn backend_name(&self) -> &'static str {
        "file"
    }
}

/// Selects the active provider based on configuration. The `env` backend is
/// the safe default and matches pre-refactor behaviour exactly.
pub fn from_env() -> std::sync::Arc<dyn SecretProvider> {
    let backend = env::var("SECRET_PROVIDER").unwrap_or_else(|_| "env".into());
    match backend.as_str() {
        "file" => {
            let root = env::var("SECRETS_DIR").unwrap_or_else(|_| "/var/secrets".into());
            tracing::info!(target = "secrets", backend = "file", root = %root, "secret provider selected");
            std::sync::Arc::new(FileSecretProvider::new(root))
        }
        other => {
            if other != "env" {
                warn!(target = "secrets", value = %other, "unknown SECRET_PROVIDER, falling back to env");
            }
            tracing::info!(target = "secrets", backend = "env", "secret provider selected");
            std::sync::Arc::new(EnvSecretProvider::new())
        }
    }
}

/// Reads `SSH_KEY_PATH`-style values (a path the caller will pass to libgit2).
/// Convenience helper retained for parity with the existing call sites.
pub fn resolve_path_from_env(name: &str) -> Option<PathBuf> {
    let raw = env::var(name).ok()?;
    if raw.is_empty() {
        None
    } else {
        Some(PathBuf::from(raw))
    }
}

/// Verify a path exists and is a regular file (used by the SSH-key resolver).
pub fn path_is_file(path: &Path) -> bool {
    path.is_file()
}

/// Synchronous helpers for call sites that cannot await (libgit2 credentials
/// callback, blocking parser plumbing). Mirrors the async trait's resolution
/// order: env first, then mounted file when `SECRET_PROVIDER=file`.
pub mod sync {
    use std::env;
    use std::fs;
    use std::path::PathBuf;

    use super::SecretKey;
    use domain::ProjectId;

    /// Lookup precedence: `<KEY>_<UPPER_SLUG>` env, `<KEY>` env,
    /// `<SECRETS_DIR>/<project_uuid>/<key>` file, `<SECRETS_DIR>/_global/<key>`
    /// file. Returns `None` when nothing is configured.
    pub fn resolve(project: Option<ProjectId>, key: &SecretKey) -> Option<String> {
        // 1. Project-scoped env override `<KEY>_<PROJECT_UUID_HEX>`.
        if let Some(p) = project {
            let suffix = p
                .as_uuid()
                .simple()
                .to_string()
                .to_ascii_uppercase();
            let scoped = format!("{}_{suffix}", key.env_var());
            if let Ok(v) = env::var(&scoped) {
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
        // 2. Plain env.
        if let Ok(v) = env::var(key.env_var()) {
            if !v.is_empty() {
                return Some(v);
            }
        }
        // 3. File backend (only when explicitly selected).
        if env::var("SECRET_PROVIDER").ok().as_deref() == Some("file") {
            let root: PathBuf = env::var("SECRETS_DIR")
                .unwrap_or_else(|_| "/var/secrets".into())
                .into();
            // Project-scoped file.
            if let Some(p) = project {
                let path = root
                    .join(p.as_uuid().to_string())
                    .join(key.as_str());
                if let Ok(bytes) = fs::read(&path) {
                    if let Ok(s) = String::from_utf8(bytes) {
                        let trimmed = s.trim_end_matches(['\n', '\r']).to_owned();
                        if !trimmed.is_empty() {
                            return Some(trimmed);
                        }
                    }
                }
            }
            // Global fallback file.
            let path = root.join("_global").join(key.as_str());
            if let Ok(bytes) = fs::read(&path) {
                if let Ok(s) = String::from_utf8(bytes) {
                    let trimmed = s.trim_end_matches(['\n', '\r']).to_owned();
                    if !trimmed.is_empty() {
                        return Some(trimmed);
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // Each test uses a unique env-var name via SecretKey::Custom so cargo's
    // parallel test runner cannot race on shared state.

    #[tokio::test]
    async fn env_provider_returns_value_when_set() {
        let key = SecretKey::Custom("MR_AI_TEST_ENV_HIT");
        unsafe { env::set_var(key.env_var(), "abc123") };
        let p = EnvSecretProvider::new();
        let v = p.get(None, &key).await.unwrap();
        assert_eq!(v, "abc123");
        unsafe { env::remove_var(key.env_var()) };
    }

    #[tokio::test]
    async fn env_provider_not_found_when_unset() {
        let key = SecretKey::Custom("MR_AI_TEST_ENV_MISS");
        unsafe { env::remove_var(key.env_var()) };
        let p = EnvSecretProvider::new();
        let err = p.get(None, &key).await.unwrap_err();
        assert!(matches!(err, SecretError::NotFound { .. }));
    }

    #[tokio::test]
    async fn file_provider_reads_global_scope() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("_global");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("git_token"), "tok-from-file\n").unwrap();

        let p = FileSecretProvider::new(tmp.path().to_path_buf());
        let v = p.get(None, &SecretKey::GitToken).await.unwrap();
        assert_eq!(v, "tok-from-file");
    }

    #[tokio::test]
    async fn file_provider_reads_project_scope() {
        let tmp = TempDir::new().unwrap();
        let pid = ProjectId::new();
        let dir = tmp.path().join(pid.as_uuid().to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ssh_key_passphrase"), "open-sesame").unwrap();

        let p = FileSecretProvider::new(tmp.path().to_path_buf());
        let v = p
            .get(Some(pid), &SecretKey::SshKeyPassphrase)
            .await
            .unwrap();
        assert_eq!(v, "open-sesame");
    }

    #[tokio::test]
    async fn file_provider_not_found_when_missing() {
        let tmp = TempDir::new().unwrap();
        let p = FileSecretProvider::new(tmp.path().to_path_buf());
        let err = p.get(None, &SecretKey::GitToken).await.unwrap_err();
        assert!(matches!(err, SecretError::NotFound { .. }));
    }
}
