mod dial;
mod ops;
mod sftp;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use zeroize::Zeroizing;

use crate::connections::{self, AuthMethod, Connection as SavedConnection};
use crate::db::{Database, DbError};
use crate::known_hosts::{self, HostKeyDecision, HostKeyError, HostKeyPrompt, HostKeyState};
use crate::session::dial::RusshDialer;
use crate::vault::{CredentialSecret, VaultError, VaultState};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub session_id: String,
    pub connection_id: String,
    pub name: String,
    pub username: String,
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStarted {
    pub session_id: String,
}

#[derive(Debug, Serialize)]
pub struct CommandError {
    pub code: &'static str,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<Box<HostKeyPrompt>>,
}

#[derive(Debug)]
enum SessionError {
    VaultLocked,
    HostKey(Box<HostKeyPrompt>),
    AuthFailed,
    Unreachable,
    NotFound,
    NothingToTrust,
    UnsupportedAuth,
    Unsupported,
    SftpUnavailable,
    InvalidHostKey,
    AlreadyTrusted,
    Database,
}

impl From<DbError> for SessionError {
    fn from(_: DbError) -> Self {
        Self::Database
    }
}

impl From<SessionError> for CommandError {
    fn from(error: SessionError) -> Self {
        let (code, message, prompt) = match error {
            SessionError::VaultLocked => {
                ("vault_locked", "Unlock the vault first.".to_string(), None)
            }
            SessionError::HostKey(prompt) => {
                let (code, message) = host_key_message(&prompt);
                (code, message, Some(prompt))
            }
            SessionError::AuthFailed => ("auth_failed", "Authentication failed.".to_string(), None),
            SessionError::Unreachable => (
                "unreachable",
                "Could not reach the server.".to_string(),
                None,
            ),
            SessionError::NotFound => ("not_found", "Connection not found.".to_string(), None),
            SessionError::NothingToTrust => (
                "not_found",
                "No host key is waiting for trust.".to_string(),
                None,
            ),
            SessionError::UnsupportedAuth => (
                "auth_failed",
                "SSH agent authentication is not available yet.".to_string(),
                None,
            ),
            SessionError::Unsupported => (
                "unsupported",
                "SSH certificates are not supported.".to_string(),
                None,
            ),
            SessionError::SftpUnavailable => (
                "sftp_unavailable",
                "The server did not open SFTP.".to_string(),
                None,
            ),
            SessionError::InvalidHostKey => (
                "invalid_host_key",
                "The fingerprint does not match the presented host key.".to_string(),
                None,
            ),
            SessionError::AlreadyTrusted => (
                "try_again",
                "The host key is already trusted. Connect again.".to_string(),
                None,
            ),
            SessionError::Database => (
                "database",
                "Could not update the session.".to_string(),
                None,
            ),
        };
        Self {
            code,
            message,
            prompt,
        }
    }
}

fn host_key_message(prompt: &HostKeyPrompt) -> (&'static str, String) {
    match prompt.decision {
        HostKeyDecision::Changed => (
            "host_key_changed",
            format!(
                "The host key changed. Trust was not replaced. {}",
                prompt.fingerprint
            ),
        ),
        HostKeyDecision::Unknown | HostKeyDecision::Trusted => (
            "host_key_unknown",
            format!(
                "This host key has not been trusted yet. {}",
                prompt.fingerprint
            ),
        ),
    }
}

pub(crate) fn accept_server_key(decision: HostKeyDecision) -> bool {
    decision == HostKeyDecision::Trusted
}

pub(crate) fn same_endpoint(
    summary: &SessionSummary,
    host: &str,
    port: u16,
    username: &str,
) -> bool {
    summary.port == port && summary.username == username && summary.host.eq_ignore_ascii_case(host)
}

pub(crate) fn matches_trusted_key(
    known: &[known_hosts::StoredHostKey],
    key_type: &str,
    public_key: &[u8],
) -> bool {
    let stored = known
        .iter()
        .find(|key| key.key_type == key_type)
        .map(|key| key.public_key.as_slice());
    accept_server_key(known_hosts::decision_for(stored, public_key))
}

enum PreparedAuth {
    Password(Zeroizing<String>),
    PrivateKey {
        pem: Zeroizing<String>,
        passphrase: Option<Zeroizing<String>>,
    },
    KeyFile {
        path: String,
        passphrase: Option<Zeroizing<String>>,
    },
}

fn prepare_auth(
    method: AuthMethod,
    key_path: Option<String>,
    secret: Option<CredentialSecret>,
) -> Result<PreparedAuth, SessionError> {
    match method {
        // Agent auth is deferred to feat/ssh-agent. Password, stored key, and key file ship here.
        AuthMethod::Agent => Err(SessionError::UnsupportedAuth),
        AuthMethod::Password => {
            let password = secret
                .and_then(|mut secret| secret.password.take())
                .filter(|password| !password.is_empty())
                .ok_or(SessionError::AuthFailed)?;
            Ok(PreparedAuth::Password(Zeroizing::new(password)))
        }
        AuthMethod::PrivateKey => {
            let mut secret = secret.ok_or(SessionError::AuthFailed)?;
            let pem = secret
                .private_key_pem
                .take()
                .filter(|pem| !pem.is_empty())
                .ok_or(SessionError::AuthFailed)?;
            let passphrase = secret
                .passphrase
                .take()
                .filter(|passphrase| !passphrase.is_empty())
                .map(Zeroizing::new);
            Ok(PreparedAuth::PrivateKey {
                pem: Zeroizing::new(pem),
                passphrase,
            })
        }
        AuthMethod::KeyFile => {
            let path = key_path
                .filter(|path| !path.is_empty() && !path.contains('\0'))
                .ok_or(SessionError::AuthFailed)?;
            let passphrase = secret
                .and_then(|mut secret| secret.passphrase.take())
                .filter(|passphrase| !passphrase.is_empty())
                .map(Zeroizing::new);
            Ok(PreparedAuth::KeyFile { path, passphrase })
        }
    }
}

struct DialTarget {
    host: String,
    port: u16,
    username: String,
    auth: PreparedAuth,
    known_keys: Vec<known_hosts::StoredHostKey>,
}

#[derive(Debug)]
enum DialError {
    Unreachable,
    AuthFailed,
    Presented {
        key_type: String,
        public_key: Vec<u8>,
    },
    Certificate,
    Sftp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LinkError {
    InvalidPath,
    NotConnected,
    NotFound,
    PermissionDenied,
    NotEmpty,
    AlreadyExists,
    NotAllowed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawMeta {
    pub is_dir: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    pub size: u64,
    pub modified: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawFile {
    pub name: String,
    pub meta: RawMeta,
}

pub(crate) trait RemoteFs: Send + Sync {
    fn list_dir<'a>(
        &'a self,
        _path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<RawFile>, LinkError>> + Send + 'a>> {
        Box::pin(async { Err(LinkError::Failed) })
    }

    fn stat_path<'a>(
        &'a self,
        _path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<RawMeta, LinkError>> + Send + 'a>> {
        Box::pin(async { Err(LinkError::Failed) })
    }

    fn lstat_path<'a>(
        &'a self,
        _path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<RawMeta, LinkError>> + Send + 'a>> {
        Box::pin(async { Err(LinkError::Failed) })
    }

    fn create_dir<'a>(
        &'a self,
        _path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), LinkError>> + Send + 'a>> {
        Box::pin(async { Err(LinkError::Failed) })
    }

    fn rename_path<'a>(
        &'a self,
        _from: &'a str,
        _to: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), LinkError>> + Send + 'a>> {
        Box::pin(async { Err(LinkError::Failed) })
    }

    fn remove_file<'a>(
        &'a self,
        _path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), LinkError>> + Send + 'a>> {
        Box::pin(async { Err(LinkError::Failed) })
    }

    fn remove_dir<'a>(
        &'a self,
        _path: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), LinkError>> + Send + 'a>> {
        Box::pin(async { Err(LinkError::Failed) })
    }
}

trait RemoteLink: RemoteFs {
    fn is_closed(&self) -> bool;
    fn close(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}

trait Dialer: Send + Sync {
    fn dial(
        &self,
        target: DialTarget,
    ) -> impl Future<Output = Result<Arc<dyn RemoteLink>, DialError>> + Send;
}

struct LiveSession {
    summary: SessionSummary,
    link: Arc<dyn RemoteLink>,
}

struct RegistryData {
    by_connection: HashMap<String, Arc<LiveSession>>,
    by_session: HashMap<String, String>,
    pending: HashMap<String, String>,
}

pub struct SessionRegistry {
    data: Mutex<RegistryData>,
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl SessionRegistry {
    fn new() -> Self {
        Self {
            data: Mutex::new(RegistryData {
                by_connection: HashMap::new(),
                by_session: HashMap::new(),
                pending: HashMap::new(),
            }),
            locks: Mutex::new(HashMap::new()),
        }
    }

    fn connection_lock(
        &self,
        connection_id: &str,
    ) -> Result<Arc<tokio::sync::Mutex<()>>, SessionError> {
        let mut locks = self.locks.lock().map_err(|_| SessionError::Database)?;
        Ok(locks
            .entry(connection_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone())
    }

    async fn connect(
        &self,
        dialer: &impl Dialer,
        database: &Database,
        vault: &VaultState,
        hosts: &HostKeyState,
        connection_id: &str,
    ) -> Result<SessionStarted, SessionError> {
        let lock = self.connection_lock(connection_id)?;
        let _guard = lock.lock().await;
        let saved = self.load_saved(database, connection_id)?;
        if let Some(existing) = self.reusable(&saved)? {
            return Ok(existing);
        }
        // A closed session, or one whose host, port, or user changed, must not
        // be returned. Close it before dialing so the old server is not left open.
        self.close_live(connection_id).await?;

        let auth = prepare_saved(database, vault, &saved)?;
        let known_keys = database.with_connection(|connection| {
            known_hosts::stored_keys(connection, &saved.host, saved.port)
                .map_err(|_| SessionError::Database)
        })?;
        let target = DialTarget {
            host: saved.host.clone(),
            port: saved.port,
            username: saved.username.clone(),
            auth,
            known_keys,
        };

        match dialer.dial(target).await {
            Ok(link) => self.store_live(database, &saved, link),
            Err(DialError::Presented {
                key_type,
                public_key,
            }) => self.stop_for_host_key(database, hosts, &saved, &key_type, &public_key),
            Err(DialError::AuthFailed) => Err(SessionError::AuthFailed),
            Err(DialError::Unreachable) => Err(SessionError::Unreachable),
            Err(DialError::Certificate) => Err(SessionError::Unsupported),
            Err(DialError::Sftp) => Err(SessionError::SftpUnavailable),
        }
    }

    async fn trust_host(
        &self,
        database: &Database,
        hosts: &HostKeyState,
        connection_id: &str,
        fingerprint: &str,
    ) -> Result<(), SessionError> {
        let lock = self.connection_lock(connection_id)?;
        let _guard = lock.lock().await;
        let pending_id = self
            .pending_for(connection_id)?
            .ok_or(SessionError::NothingToTrust)?;
        database.with_connection(|connection| {
            hosts
                .confirm_fingerprint(connection, &pending_id, fingerprint)
                .map_err(trust_error)
        })?;
        self.clear_pending_if(connection_id, &pending_id)
    }

    async fn delete_connection(
        &self,
        database: &Database,
        connection_id: &str,
    ) -> Result<(), SessionError> {
        let lock = self.connection_lock(connection_id)?;
        let _guard = lock.lock().await;
        self.close_live(connection_id).await?;
        self.clear_pending(connection_id)?;
        database.with_connection(|connection| {
            connections::remove(connection, connection_id).map_err(remove_error)
        })
    }

    async fn disconnect(&self, session_id: &str) -> Result<(), SessionError> {
        let connection_id = self
            .connection_for_session(session_id)?
            .ok_or(SessionError::NotFound)?;
        let lock = self.connection_lock(&connection_id)?;
        let _guard = lock.lock().await;
        let Some(live) = self.take_session(session_id)? else {
            return Err(SessionError::NotFound);
        };
        live.link.close().await;
        Ok(())
    }

    async fn read_dir(&self, session_id: &str, path: &str) -> Result<Vec<RawFile>, LinkError> {
        let link = self.link_for(session_id, path)?;
        link.list_dir(path).await
    }

    async fn stat_path(&self, session_id: &str, path: &str) -> Result<RawMeta, LinkError> {
        let link = self.link_for(session_id, path)?;
        link.stat_path(path).await
    }

    async fn create_dir(&self, session_id: &str, path: &str) -> Result<(), LinkError> {
        ops::plan_mkdir(path)?;
        let link = self.link_for(session_id, path)?;
        match link.lstat_path(path).await {
            Ok(_) => Err(LinkError::AlreadyExists),
            Err(LinkError::NotFound) => link.create_dir(path).await,
            Err(error) => Err(error),
        }
    }

    async fn rename_path(&self, session_id: &str, from: &str, to: &str) -> Result<(), LinkError> {
        sftp::require_absolute_remote_path(to).map_err(|_| LinkError::InvalidPath)?;
        match ops::plan_rename(from, to)? {
            ops::RenamePlan::Unchanged => {
                let _link = self.link_for(session_id, from)?;
                Ok(())
            }
            ops::RenamePlan::Proceed => {
                let link = self.link_for(session_id, from)?;
                match link.lstat_path(to).await {
                    Ok(_) => Err(LinkError::AlreadyExists),
                    Err(LinkError::NotFound) => link.rename_path(from, to).await,
                    Err(error) => Err(error),
                }
            }
        }
    }

    async fn remove_path(&self, session_id: &str, path: &str) -> Result<(), LinkError> {
        if path == "/" {
            return Err(LinkError::NotAllowed);
        }
        let link = self.link_for(session_id, path)?;
        let meta = link.lstat_path(path).await?;
        let names = if ops::is_real_directory(&meta) {
            link.list_dir(path)
                .await?
                .into_iter()
                .map(|file| file.name)
                .collect()
        } else {
            Vec::new()
        };
        match ops::plan_delete(path, &meta, &names)? {
            ops::DeleteAction::RemoveFile => link.remove_file(path).await,
            ops::DeleteAction::RemoveEmptyDir => link.remove_dir(path).await,
        }
    }

    fn link_for(&self, session_id: &str, path: &str) -> Result<Arc<dyn RemoteLink>, LinkError> {
        sftp::require_absolute_remote_path(path).map_err(|_| LinkError::InvalidPath)?;
        let data = self.data.lock().map_err(|_| LinkError::Failed)?;
        let connection_id = data
            .by_session
            .get(session_id)
            .ok_or(LinkError::NotConnected)?;
        let live = data
            .by_connection
            .get(connection_id)
            .ok_or(LinkError::NotConnected)?;
        if live.link.is_closed() {
            return Err(LinkError::NotConnected);
        }
        Ok(live.link.clone())
    }

    fn list(&self) -> Result<Vec<SessionSummary>, SessionError> {
        let mut data = self.data.lock().map_err(|_| SessionError::Database)?;
        let closed: Vec<String> = data
            .by_connection
            .iter()
            .filter(|(_, live)| live.link.is_closed())
            .map(|(id, _)| id.clone())
            .collect();
        for connection_id in closed {
            if let Some(live) = data.by_connection.remove(&connection_id) {
                data.by_session.remove(&live.summary.session_id);
            }
        }
        let mut sessions: Vec<_> = data
            .by_connection
            .values()
            .map(|live| live.summary.clone())
            .collect();
        sessions.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then(left.session_id.cmp(&right.session_id))
        });
        Ok(sessions)
    }

    fn reusable(&self, saved: &SavedConnection) -> Result<Option<SessionStarted>, SessionError> {
        let Some(existing) = self.live(&saved.id)? else {
            return Ok(None);
        };
        if existing.link.is_closed()
            || !same_endpoint(&existing.summary, &saved.host, saved.port, &saved.username)
        {
            return Ok(None);
        }
        if existing.summary.name != saved.name {
            self.rename(&saved.id, &saved.name)?;
        }
        Ok(Some(SessionStarted {
            session_id: existing.summary.session_id.clone(),
        }))
    }

    fn stop_for_host_key(
        &self,
        database: &Database,
        hosts: &HostKeyState,
        saved: &SavedConnection,
        key_type: &str,
        public_key: &[u8],
    ) -> Result<SessionStarted, SessionError> {
        let prompt = database.with_connection(|connection| {
            hosts
                .present(connection, &saved.host, saved.port, key_type, public_key)
                .map_err(present_error)
        })?;
        if prompt.decision == HostKeyDecision::Trusted {
            return Err(SessionError::AlreadyTrusted);
        }
        self.remember_pending(&saved.id, &prompt.pending_id)?;
        Err(SessionError::HostKey(Box::new(prompt)))
    }

    fn load_saved(
        &self,
        database: &Database,
        connection_id: &str,
    ) -> Result<SavedConnection, SessionError> {
        database.with_connection(|connection| {
            connections::saved(connection, connection_id)
                .map_err(|_| SessionError::Database)?
                .ok_or(SessionError::NotFound)
        })
    }

    fn store_live(
        &self,
        database: &Database,
        saved: &SavedConnection,
        link: Arc<dyn RemoteLink>,
    ) -> Result<SessionStarted, SessionError> {
        let session_id = uuid::Uuid::new_v4().to_string();
        let summary = SessionSummary {
            session_id: session_id.clone(),
            connection_id: saved.id.clone(),
            name: saved.name.clone(),
            username: saved.username.clone(),
            host: saved.host.clone(),
            port: saved.port,
        };
        {
            let mut data = self.data.lock().map_err(|_| SessionError::Database)?;
            data.by_session.insert(session_id.clone(), saved.id.clone());
            data.by_connection
                .insert(saved.id.clone(), Arc::new(LiveSession { summary, link }));
            data.pending.remove(&saved.id);
        }
        let _ = database.with_connection(|connection| {
            connections::touch_used(connection, &saved.id).map_err(|_| SessionError::Database)
        });
        Ok(SessionStarted { session_id })
    }

    fn live(&self, connection_id: &str) -> Result<Option<Arc<LiveSession>>, SessionError> {
        let data = self.data.lock().map_err(|_| SessionError::Database)?;
        Ok(data.by_connection.get(connection_id).cloned())
    }

    fn rename(&self, connection_id: &str, name: &str) -> Result<(), SessionError> {
        let mut data = self.data.lock().map_err(|_| SessionError::Database)?;
        let Some(live) = data.by_connection.get(connection_id).cloned() else {
            return Ok(());
        };
        let mut summary = live.summary.clone();
        summary.name = name.to_string();
        data.by_connection.insert(
            connection_id.to_string(),
            Arc::new(LiveSession {
                summary,
                link: live.link.clone(),
            }),
        );
        Ok(())
    }

    async fn close_live(&self, connection_id: &str) -> Result<(), SessionError> {
        let Some(live) = self.take_connection(connection_id)? else {
            return Ok(());
        };
        live.link.close().await;
        Ok(())
    }

    fn take_connection(
        &self,
        connection_id: &str,
    ) -> Result<Option<Arc<LiveSession>>, SessionError> {
        let mut data = self.data.lock().map_err(|_| SessionError::Database)?;
        let Some(live) = data.by_connection.remove(connection_id) else {
            return Ok(None);
        };
        data.by_session.remove(&live.summary.session_id);
        Ok(Some(live))
    }

    fn remember_pending(&self, connection_id: &str, pending_id: &str) -> Result<(), SessionError> {
        let mut data = self.data.lock().map_err(|_| SessionError::Database)?;
        data.pending
            .insert(connection_id.to_string(), pending_id.to_string());
        Ok(())
    }

    fn pending_for(&self, connection_id: &str) -> Result<Option<String>, SessionError> {
        let data = self.data.lock().map_err(|_| SessionError::Database)?;
        Ok(data.pending.get(connection_id).cloned())
    }

    fn clear_pending(&self, connection_id: &str) -> Result<(), SessionError> {
        let mut data = self.data.lock().map_err(|_| SessionError::Database)?;
        data.pending.remove(connection_id);
        Ok(())
    }

    fn clear_pending_if(&self, connection_id: &str, pending_id: &str) -> Result<(), SessionError> {
        let mut data = self.data.lock().map_err(|_| SessionError::Database)?;
        if data
            .pending
            .get(connection_id)
            .is_some_and(|current| current == pending_id)
        {
            data.pending.remove(connection_id);
        }
        Ok(())
    }

    fn connection_for_session(&self, session_id: &str) -> Result<Option<String>, SessionError> {
        let data = self.data.lock().map_err(|_| SessionError::Database)?;
        Ok(data.by_session.get(session_id).cloned())
    }

    fn take_session(&self, session_id: &str) -> Result<Option<Arc<LiveSession>>, SessionError> {
        let mut data = self.data.lock().map_err(|_| SessionError::Database)?;
        let Some(connection_id) = data.by_session.remove(session_id) else {
            return Ok(None);
        };
        Ok(data.by_connection.remove(&connection_id))
    }
}

fn prepare_saved(
    database: &Database,
    vault: &VaultState,
    saved: &SavedConnection,
) -> Result<PreparedAuth, SessionError> {
    if saved.auth_method == AuthMethod::Agent {
        return Err(SessionError::UnsupportedAuth);
    }
    let locked = vault.is_locked().map_err(vault_error)?;
    let has_secret = database
        .with_connection(|connection| vault.has_secret(connection, &saved.id))
        .map_err(vault_error)?;
    if locked && needs_vault(saved.auth_method, has_secret) {
        return Err(SessionError::VaultLocked);
    }
    let secret = if locked {
        None
    } else {
        database
            .with_connection(|connection| vault.load_secret(connection, &saved.id))
            .map_err(vault_error)?
    };
    prepare_auth(saved.auth_method, saved.key_path.clone(), secret)
}

fn needs_vault(method: AuthMethod, has_secret: bool) -> bool {
    match method {
        AuthMethod::Password | AuthMethod::PrivateKey => true,
        AuthMethod::KeyFile => has_secret,
        AuthMethod::Agent => false,
    }
}

fn remove_error(error: connections::RemoveError) -> SessionError {
    match error {
        connections::RemoveError::NotFound => SessionError::NotFound,
        connections::RemoveError::Database => SessionError::Database,
    }
}

fn vault_error(error: VaultError) -> SessionError {
    match error {
        VaultError::Locked => SessionError::VaultLocked,
        VaultError::NotFound => SessionError::NotFound,
        _ => SessionError::Database,
    }
}

fn present_error(error: HostKeyError) -> SessionError {
    match error {
        HostKeyError::Invalid(_) => SessionError::InvalidHostKey,
        HostKeyError::Database => SessionError::Database,
        HostKeyError::Unknown | HostKeyError::Changed | HostKeyError::NotFound => {
            SessionError::Database
        }
    }
}

fn trust_error(error: HostKeyError) -> SessionError {
    match error {
        HostKeyError::NotFound => SessionError::NothingToTrust,
        HostKeyError::Invalid(_) => SessionError::InvalidHostKey,
        HostKeyError::Database => SessionError::Database,
        HostKeyError::Changed | HostKeyError::Unknown => SessionError::InvalidHostKey,
    }
}

pub struct SessionState {
    registry: SessionRegistry,
    dialer: RusshDialer,
}

impl SessionState {
    pub fn new() -> Self {
        Self {
            registry: SessionRegistry::new(),
            dialer: RusshDialer,
        }
    }

    async fn connect(
        &self,
        database: &Database,
        vault: &VaultState,
        hosts: &HostKeyState,
        connection_id: &str,
    ) -> Result<SessionStarted, SessionError> {
        self.registry
            .connect(&self.dialer, database, vault, hosts, connection_id)
            .await
    }

    pub(crate) async fn read_dir(
        &self,
        session_id: &str,
        path: &str,
    ) -> Result<Vec<RawFile>, LinkError> {
        self.registry.read_dir(session_id, path).await
    }

    pub(crate) async fn stat_path(
        &self,
        session_id: &str,
        path: &str,
    ) -> Result<RawMeta, LinkError> {
        self.registry.stat_path(session_id, path).await
    }

    pub(crate) async fn create_dir(&self, session_id: &str, path: &str) -> Result<(), LinkError> {
        self.registry.create_dir(session_id, path).await
    }

    pub(crate) async fn rename_path(
        &self,
        session_id: &str,
        from: &str,
        to: &str,
    ) -> Result<(), LinkError> {
        self.registry.rename_path(session_id, from, to).await
    }

    pub(crate) async fn remove_path(&self, session_id: &str, path: &str) -> Result<(), LinkError> {
        self.registry.remove_path(session_id, path).await
    }

    pub async fn delete_connection(
        &self,
        database: &Database,
        connection_id: &str,
    ) -> Result<(), crate::connections::CommandError> {
        self.registry
            .delete_connection(database, connection_id)
            .await
            .map_err(|error| {
                let command = CommandError::from(error);
                crate::connections::CommandError {
                    code: command.code,
                    message: command.message,
                }
            })
    }
}

impl Default for SessionState {
    fn default() -> Self {
        Self::new()
    }
}

#[tauri::command]
pub async fn session_connect(
    app: tauri::AppHandle,
    database: tauri::State<'_, Database>,
    vault: tauri::State<'_, VaultState>,
    hosts: tauri::State<'_, HostKeyState>,
    sessions: tauri::State<'_, SessionState>,
    connection_id: String,
) -> Result<SessionStarted, CommandError> {
    match sessions
        .connect(&database, &vault, &hosts, &connection_id)
        .await
    {
        Ok(started) => Ok(started),
        Err(SessionError::HostKey(prompt)) => {
            let _ = tauri::Emitter::emit(&app, "hostkey://prompt", prompt.as_ref());
            Err(CommandError::from(SessionError::HostKey(prompt)))
        }
        Err(error) => Err(CommandError::from(error)),
    }
}

#[tauri::command]
pub async fn session_trust_host(
    database: tauri::State<'_, Database>,
    hosts: tauri::State<'_, HostKeyState>,
    sessions: tauri::State<'_, SessionState>,
    connection_id: String,
    fingerprint: String,
) -> Result<(), CommandError> {
    sessions
        .registry
        .trust_host(&database, &hosts, &connection_id, &fingerprint)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub async fn session_disconnect(
    sessions: tauri::State<'_, SessionState>,
    session_id: String,
) -> Result<(), CommandError> {
    sessions
        .registry
        .disconnect(&session_id)
        .await
        .map_err(CommandError::from)
}

#[tauri::command]
pub fn session_list(
    sessions: tauri::State<'_, SessionState>,
) -> Result<Vec<SessionSummary>, CommandError> {
    sessions.registry.list().map_err(CommandError::from)
}

#[cfg(test)]
mod tests {
    use super::{
        CommandError, DialError, DialTarget, Dialer, LinkError, RawFile, RawMeta, RemoteFs,
        RemoteLink, SessionError, SessionRegistry, SessionSummary, accept_server_key,
        matches_trusted_key, prepare_auth, same_endpoint,
    };
    use crate::connections::AuthMethod;
    use crate::db::apply_migrations;
    use crate::known_hosts::{HostKeyDecision, HostKeyState, StoredHostKey, fingerprint};
    use crate::vault::crypto::test_kdf;
    use crate::vault::keystore::MemoryKeyStore;
    use crate::vault::{CredentialSecret, VaultState};
    use rusqlite::Connection as Sqlite;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct FakeLink {
        closed: Arc<AtomicBool>,
    }

    impl RemoteFs for FakeLink {}

    impl RemoteLink for FakeLink {
        fn is_closed(&self) -> bool {
            self.closed.load(Ordering::SeqCst)
        }

        fn close(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            self.closed.store(true, Ordering::SeqCst);
            Box::pin(async {})
        }
    }

    enum Script {
        Connected,
        Presented {
            key_type: String,
            public_key: Vec<u8>,
        },
        AuthFailed,
        Unreachable,
        Certificate,
    }

    struct ScriptedDialer {
        calls: AtomicUsize,
        delay: Duration,
        scripts: Mutex<VecDeque<Script>>,
        last_closed: Mutex<Option<Arc<AtomicBool>>>,
        saw_known: AtomicUsize,
    }

    impl ScriptedDialer {
        fn new(scripts: Vec<Script>) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                delay: Duration::ZERO,
                scripts: Mutex::new(scripts.into()),
                last_closed: Mutex::new(None),
                saw_known: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Dialer for ScriptedDialer {
        async fn dial(&self, target: DialTarget) -> Result<Arc<dyn RemoteLink>, DialError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.saw_known
                .fetch_add(target.known_keys.len(), Ordering::SeqCst);
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            let script = self
                .scripts
                .lock()
                .expect("script")
                .pop_front()
                .unwrap_or(Script::Connected);
            match script {
                Script::Connected => {
                    let closed = Arc::new(AtomicBool::new(false));
                    *self.last_closed.lock().expect("closed") = Some(closed.clone());
                    Ok(Arc::new(FakeLink { closed }))
                }
                Script::Presented {
                    key_type,
                    public_key,
                } => Err(DialError::Presented {
                    key_type,
                    public_key,
                }),
                Script::AuthFailed => Err(DialError::AuthFailed),
                Script::Unreachable => Err(DialError::Unreachable),
                Script::Certificate => Err(DialError::Certificate),
            }
        }
    }

    struct Fixture {
        database: crate::db::Database,
        vault: VaultState,
        hosts: HostKeyState,
        registry: SessionRegistry,
        connection_id: String,
    }

    fn open_sqlite() -> Sqlite {
        let mut connection = Sqlite::open_in_memory().expect("memory database");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        apply_migrations(&mut connection).expect("migrate");
        connection
    }

    fn fixture(method: &str, key_path: Option<&str>) -> Fixture {
        let sqlite = open_sqlite();
        let id = "conn-1".to_string();
        sqlite
            .execute(
                "INSERT INTO connections (
                    id, name, host, port, username, auth_method, key_path, created_at, updated_at
                 ) VALUES (?1, 'prod-telegraf', 'Example.COM', 22, 'ubuntu', ?2, ?3, datetime('now'), datetime('now'))",
                rusqlite::params![id, method, key_path],
            )
            .expect("insert connection");
        let vault = VaultState::new(Box::new(MemoryKeyStore::unavailable()), test_kdf());
        vault
            .init_password_on(&sqlite, "correct horse")
            .expect("init vault");
        let database = crate::db::Database::from_connection(sqlite);
        Fixture {
            database,
            vault,
            hosts: HostKeyState::new(),
            registry: SessionRegistry::new(),
            connection_id: id,
        }
    }

    fn save_password(fixture: &Fixture, password: &str) {
        fixture
            .database
            .with_connection(|connection| {
                fixture.vault.set_credential_on(
                    connection,
                    &fixture.connection_id,
                    &CredentialSecret {
                        password: Some(password.to_string()),
                        private_key_pem: None,
                        passphrase: None,
                    },
                )
            })
            .expect("save secret");
    }

    #[test]
    fn only_a_trusted_host_key_may_continue() {
        assert!(!accept_server_key(HostKeyDecision::Unknown));
        assert!(!accept_server_key(HostKeyDecision::Changed));
        assert!(accept_server_key(HostKeyDecision::Trusted));

        let known = [StoredHostKey {
            key_type: "ssh-ed25519".to_string(),
            public_key: b"key-a".to_vec(),
        }];
        assert!(matches_trusted_key(&known, "ssh-ed25519", b"key-a"));
        assert!(!matches_trusted_key(&known, "ssh-ed25519", b"key-b"));
        assert!(!matches_trusted_key(&known, "ssh-rsa", b"key-a"));
        assert!(!matches_trusted_key(&[], "ssh-ed25519", b"key-a"));
    }

    #[test]
    fn auth_errors_do_not_include_the_secret() {
        let Err(error) = prepare_auth(AuthMethod::Password, None, None) else {
            panic!("missing password should fail");
        };
        let command = CommandError::from(error);
        assert_eq!(command.code, "auth_failed");
        assert!(!command.message.contains("hunter2-secret"));

        let nul = prepare_auth(
            AuthMethod::KeyFile,
            Some("/home/ubuntu/.ssh/id_ed25519\0".to_string()),
            None,
        );
        assert!(nul.is_err());
        assert!(prepare_auth(AuthMethod::Agent, None, None).is_err());
        let agent = CommandError::from(SessionError::UnsupportedAuth);
        assert_eq!(agent.code, "auth_failed");
        assert!(!agent.message.contains("hunter2-secret"));
    }

    #[tokio::test]
    async fn reuses_a_live_session_and_does_not_dial_again() {
        let fixture = fixture("password", None);
        save_password(&fixture, "hunter2-secret");
        let dialer = ScriptedDialer::new(vec![Script::Connected]);

        let first = fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect("connect");
        let second = fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect("reuse");

        assert_eq!(first.session_id, second.session_id);
        assert_eq!(dialer.calls(), 1);
        assert_eq!(fixture.registry.list().expect("list").len(), 1);
        let used: Option<String> = fixture
            .database
            .with_connection(|connection| {
                connection
                    .query_row(
                        "SELECT last_used_at FROM connections WHERE id = ?1",
                        [&fixture.connection_id],
                        |row| row.get(0),
                    )
                    .map_err(|_| SessionError::Database)
            })
            .expect("used");
        assert!(used.is_some());
    }

    #[test]
    fn endpoint_match_ignores_host_case_only() {
        let summary = SessionSummary {
            session_id: "ses".to_string(),
            connection_id: "conn-1".to_string(),
            name: "prod-telegraf".to_string(),
            username: "ubuntu".to_string(),
            host: "Example.COM".to_string(),
            port: 22,
        };
        assert!(same_endpoint(&summary, "example.com", 22, "ubuntu"));
        assert!(!same_endpoint(&summary, "other.example", 22, "ubuntu"));
        assert!(!same_endpoint(&summary, "example.com", 2222, "ubuntu"));
        assert!(!same_endpoint(&summary, "example.com", 22, "root"));
    }

    #[tokio::test]
    async fn a_changed_host_is_not_reused_and_the_old_link_is_closed() {
        let fixture = fixture("password", None);
        save_password(&fixture, "hunter2-secret");
        let dialer = ScriptedDialer::new(vec![Script::Connected, Script::Connected]);
        let first = fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect("first");
        let first_closed = dialer
            .last_closed
            .lock()
            .expect("closed")
            .clone()
            .expect("link");
        fixture
            .database
            .with_connection(|connection| {
                connection
                    .execute(
                        "UPDATE connections SET host = 'other.example', name = 'renamed' WHERE id = ?1",
                        [&fixture.connection_id],
                    )
                    .map_err(|_| SessionError::Database)?;
                Ok::<(), SessionError>(())
            })
            .expect("retarget");
        let second = fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect("second");
        assert_ne!(first.session_id, second.session_id);
        assert!(first_closed.load(Ordering::SeqCst));
        assert_eq!(dialer.calls(), 2);
        assert_eq!(fixture.registry.list().expect("list")[0].name, "renamed");
        assert!(!format!("{second:?}").contains("hunter2-secret"));
    }

    #[tokio::test]
    async fn a_case_only_host_change_reuses_the_session_and_refreshes_the_name() {
        let fixture = fixture("password", None);
        save_password(&fixture, "hunter2-secret");
        let dialer = ScriptedDialer::new(vec![Script::Connected]);
        let first = fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect("first");
        fixture
            .database
            .with_connection(|connection| {
                connection
                    .execute(
                        "UPDATE connections SET host = 'example.com', name = 'renamed' WHERE id = ?1",
                        [&fixture.connection_id],
                    )
                    .map_err(|_| SessionError::Database)?;
                Ok::<(), SessionError>(())
            })
            .expect("rename");
        let second = fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect("reuse");
        assert_eq!(first.session_id, second.session_id);
        assert_eq!(dialer.calls(), 1);
        assert_eq!(fixture.registry.list().expect("list")[0].name, "renamed");
        assert_eq!(
            fixture.registry.list().expect("list")[0].host,
            "Example.COM"
        );
    }

    #[tokio::test]
    async fn deleting_a_connection_closes_its_session() {
        let fixture = fixture("password", None);
        save_password(&fixture, "hunter2-secret");
        let dialer = ScriptedDialer::new(vec![Script::Connected]);
        fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect("connect");
        let closed = dialer
            .last_closed
            .lock()
            .expect("closed")
            .clone()
            .expect("link");
        fixture
            .registry
            .delete_connection(&fixture.database, &fixture.connection_id)
            .await
            .expect("delete");
        assert!(closed.load(Ordering::SeqCst));
        assert!(fixture.registry.list().expect("empty").is_empty());
        let gone = fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect_err("deleted");
        assert!(matches!(gone, SessionError::NotFound));
        assert_eq!(dialer.calls(), 1);
    }

    #[tokio::test]
    async fn concurrent_connect_dials_once() {
        let fixture = fixture("password", None);
        save_password(&fixture, "hunter2-secret");
        let mut dialer = ScriptedDialer::new(vec![Script::Connected]);
        dialer.delay = Duration::from_millis(50);
        let dialer = Arc::new(dialer);
        let registry = Arc::new(fixture.registry);
        let database = Arc::new(fixture.database);
        let vault = Arc::new(fixture.vault);
        let hosts = Arc::new(fixture.hosts);

        let mut tasks = Vec::new();
        for _ in 0..2 {
            let dialer = dialer.clone();
            let registry = registry.clone();
            let database = database.clone();
            let vault = vault.clone();
            let hosts = hosts.clone();
            tasks.push(tokio::spawn(async move {
                registry
                    .connect(&*dialer, &database, &vault, &hosts, "conn-1")
                    .await
                    .expect("connect")
                    .session_id
            }));
        }
        let mut ids = Vec::new();
        for task in tasks {
            ids.push(task.await.expect("task"));
        }
        assert_eq!(ids[0], ids[1]);
        assert_eq!(dialer.calls(), 1);
    }

    #[tokio::test]
    async fn a_closed_session_is_not_reused() {
        let fixture = fixture("password", None);
        save_password(&fixture, "hunter2-secret");
        let dialer = ScriptedDialer::new(vec![Script::Connected, Script::Connected]);
        let first = fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect("first");
        dialer
            .last_closed
            .lock()
            .expect("closed")
            .as_ref()
            .expect("link")
            .store(true, Ordering::SeqCst);
        assert!(fixture.registry.list().expect("drops closed").is_empty());
        let second = fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect("second");
        assert_ne!(first.session_id, second.session_id);
        assert_eq!(dialer.calls(), 2);
    }

    #[tokio::test]
    async fn disconnect_then_connect_dials_again() {
        let fixture = fixture("password", None);
        save_password(&fixture, "hunter2-secret");
        let dialer = ScriptedDialer::new(vec![Script::Connected, Script::Connected]);
        let first = fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect("first");
        fixture
            .registry
            .disconnect(&first.session_id)
            .await
            .expect("disconnect");
        assert!(fixture.registry.list().expect("empty").is_empty());
        let second = fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect("second");
        assert_ne!(first.session_id, second.session_id);
        assert_eq!(dialer.calls(), 2);
    }

    #[tokio::test]
    async fn unknown_host_key_does_not_create_a_session_or_record_trust() {
        let fixture = fixture("password", None);
        save_password(&fixture, "hunter2-secret");
        let dialer = ScriptedDialer::new(vec![Script::Presented {
            key_type: "ssh-ed25519".to_string(),
            public_key: b"key-a".to_vec(),
        }]);
        let error = fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect_err("unknown");
        let command = CommandError::from(error);
        assert_eq!(command.code, "host_key_unknown");
        assert!(command.message.contains(&fingerprint(b"key-a")));
        assert_eq!(
            command
                .prompt
                .as_ref()
                .map(|prompt| prompt.fingerprint.as_str()),
            Some(fingerprint(b"key-a").as_str())
        );
        assert!(!command.message.contains("hunter2-secret"));
        assert!(!format!("{:?}", command.prompt).contains("hunter2-secret"));
        assert!(fixture.registry.list().expect("list").is_empty());
        let stored: i64 = fixture
            .database
            .with_connection(|connection| {
                connection
                    .query_row("SELECT COUNT(*) FROM known_hosts", [], |row| row.get(0))
                    .map_err(|_| SessionError::Database)
            })
            .expect("count");
        assert_eq!(stored, 0);
    }

    #[tokio::test]
    async fn changed_host_key_is_not_replaced_until_trust_is_explicit() {
        let fixture = fixture("password", None);
        save_password(&fixture, "hunter2-secret");
        fixture
            .database
            .with_connection(|connection| {
                let prompt = fixture
                    .hosts
                    .present(connection, "example.com", 22, "ssh-ed25519", b"key-a")
                    .expect("present original");
                assert_eq!(prompt.decision, HostKeyDecision::Unknown);
                fixture
                    .hosts
                    .confirm_fingerprint(connection, &prompt.pending_id, &prompt.fingerprint)
                    .expect("trust original");
                Ok::<(), SessionError>(())
            })
            .expect("seed trust");

        let dialer = ScriptedDialer::new(vec![
            Script::Presented {
                key_type: "ssh-ed25519".to_string(),
                public_key: b"key-b".to_vec(),
            },
            Script::Connected,
        ]);
        let error = fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect_err("changed");
        let command = CommandError::from(error);
        assert_eq!(command.code, "host_key_changed");
        assert!(dialer.saw_known.load(Ordering::SeqCst) >= 1);
        let still_original: String = fixture
            .database
            .with_connection(|connection| {
                connection
                    .query_row(
                        "SELECT fingerprint FROM known_hosts WHERE host = 'example.com'",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|_| SessionError::Database)
            })
            .expect("fingerprint");
        assert_eq!(still_original, fingerprint(b"key-a"));

        let mismatch = fixture
            .registry
            .trust_host(
                &fixture.database,
                &fixture.hosts,
                &fixture.connection_id,
                "SHA256:not-the-key",
            )
            .await
            .expect_err("mismatch");
        assert!(matches!(mismatch, SessionError::InvalidHostKey));
        assert_eq!(dialer.calls(), 1);

        fixture
            .registry
            .trust_host(
                &fixture.database,
                &fixture.hosts,
                &fixture.connection_id,
                &fingerprint(b"key-b"),
            )
            .await
            .expect("replace");
        let again = fixture
            .registry
            .trust_host(
                &fixture.database,
                &fixture.hosts,
                &fixture.connection_id,
                &fingerprint(b"key-b"),
            )
            .await
            .expect_err("pending is consumed");
        assert!(matches!(again, SessionError::NothingToTrust));
        let replaced: String = fixture
            .database
            .with_connection(|connection| {
                connection
                    .query_row(
                        "SELECT fingerprint FROM known_hosts WHERE host = 'example.com'",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|_| SessionError::Database)
            })
            .expect("replaced");
        assert_eq!(replaced, fingerprint(b"key-b"));
        assert_eq!(dialer.calls(), 1);

        fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect("connected after trust");
        assert_eq!(dialer.calls(), 2);
    }

    #[tokio::test]
    async fn locked_vault_agent_and_missing_connection_do_not_dial() {
        let locked = fixture("password", None);
        save_password(&locked, "hunter2-secret");
        let fresh = VaultState::new(Box::new(MemoryKeyStore::unavailable()), test_kdf());
        let dialer = ScriptedDialer::new(vec![Script::Connected]);
        let error = locked
            .registry
            .connect(
                &dialer,
                &locked.database,
                &fresh,
                &locked.hosts,
                &locked.connection_id,
            )
            .await
            .expect_err("locked");
        assert!(matches!(error, SessionError::VaultLocked));
        assert_eq!(dialer.calls(), 0);

        let agent = fixture("agent", None);
        let error = agent
            .registry
            .connect(
                &dialer,
                &agent.database,
                &agent.vault,
                &agent.hosts,
                &agent.connection_id,
            )
            .await
            .expect_err("agent");
        assert!(matches!(error, SessionError::UnsupportedAuth));
        assert_eq!(dialer.calls(), 0);
        let message = CommandError::from(error).message;
        assert!(!message.contains("hunter2-secret"));

        let missing = agent
            .registry
            .connect(
                &dialer,
                &agent.database,
                &agent.vault,
                &agent.hosts,
                "missing",
            )
            .await
            .expect_err("missing");
        assert!(matches!(missing, SessionError::NotFound));
        assert_eq!(dialer.calls(), 0);
    }

    #[tokio::test]
    async fn key_file_without_a_passphrase_does_not_need_an_unlocked_vault() {
        let fixture = fixture("key_file", Some("/home/ubuntu/.ssh/id_ed25519"));
        let fresh = VaultState::new(Box::new(MemoryKeyStore::unavailable()), test_kdf());
        let dialer = ScriptedDialer::new(vec![Script::Connected]);
        fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fresh,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect("key file");
        assert_eq!(dialer.calls(), 1);

        fixture
            .database
            .with_connection(|connection| {
                fixture.vault.set_credential_on(
                    connection,
                    &fixture.connection_id,
                    &CredentialSecret {
                        password: None,
                        private_key_pem: None,
                        passphrase: Some("hunter2-phrase".to_string()),
                    },
                )
            })
            .expect("save passphrase");
        fixture
            .registry
            .disconnect(&fixture.registry.list().expect("list")[0].session_id.clone())
            .await
            .expect("disconnect");
        let blocked = fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fresh,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect_err("locked passphrase");
        assert!(matches!(blocked, SessionError::VaultLocked));
        assert_eq!(dialer.calls(), 1);
        assert!(
            !CommandError::from(blocked)
                .message
                .contains("hunter2-phrase")
        );
    }

    #[tokio::test]
    async fn auth_failure_unreachable_and_certificate_do_not_keep_a_session() {
        let fixture = fixture("password", None);
        save_password(&fixture, "hunter2-secret");
        let dialer = ScriptedDialer::new(vec![
            Script::AuthFailed,
            Script::Unreachable,
            Script::Certificate,
        ]);
        let auth = fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect_err("auth");
        assert_eq!(CommandError::from(auth).code, "auth_failed");
        let unreachable = fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect_err("unreachable");
        assert_eq!(CommandError::from(unreachable).code, "unreachable");
        let certificate = fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect_err("certificate");
        assert_eq!(CommandError::from(certificate).code, "unsupported");
        assert!(fixture.registry.list().expect("empty").is_empty());
        let stored: i64 = fixture
            .database
            .with_connection(|connection| {
                connection
                    .query_row("SELECT COUNT(*) FROM known_hosts", [], |row| row.get(0))
                    .map_err(|_| SessionError::Database)
            })
            .expect("count");
        assert_eq!(stored, 0);
    }

    struct ListingLink {
        calls: AtomicUsize,
        closed: AtomicBool,
    }

    impl RemoteFs for ListingLink {
        fn list_dir<'a>(
            &'a self,
            path: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<RawFile>, LinkError>> + Send + 'a>,
        > {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let path = path.to_string();
            Box::pin(async move {
                assert!(path.starts_with('/'));
                Ok(vec![RawFile {
                    name: "telegraf.conf".to_string(),
                    meta: RawMeta {
                        is_dir: false,
                        is_file: true,
                        is_symlink: false,
                        size: 12,
                        modified: Some(1),
                    },
                }])
            })
        }
    }

    impl RemoteLink for ListingLink {
        fn is_closed(&self) -> bool {
            self.closed.load(Ordering::SeqCst)
        }

        fn close(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            self.closed.store(true, Ordering::SeqCst);
            Box::pin(async {})
        }
    }

    struct ListingDialer {
        link: Arc<ListingLink>,
    }

    impl Dialer for ListingDialer {
        async fn dial(&self, _target: DialTarget) -> Result<Arc<dyn RemoteLink>, DialError> {
            Ok(self.link.clone())
        }
    }

    #[tokio::test]
    async fn listing_uses_the_live_link_and_a_bad_path_does_not() {
        let fixture = fixture("password", None);
        save_password(&fixture, "hunter2-secret");
        let link = Arc::new(ListingLink {
            calls: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
        });
        let dialer = ListingDialer { link: link.clone() };
        let started = fixture
            .registry
            .connect(
                &dialer,
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect("connect");

        let missing = fixture
            .registry
            .read_dir("missing", "/etc")
            .await
            .expect_err("missing");
        assert!(matches!(missing, LinkError::NotConnected));
        assert_eq!(link.calls.load(Ordering::SeqCst), 0);

        let relative = fixture
            .registry
            .read_dir(&started.session_id, "relative.conf")
            .await
            .expect_err("relative");
        assert!(matches!(relative, LinkError::InvalidPath));
        let nul = fixture
            .registry
            .read_dir(&started.session_id, "/etc/\0telegraf")
            .await
            .expect_err("nul");
        assert!(matches!(nul, LinkError::InvalidPath));
        assert!(!format!("{nul:?}").contains("hunter2-secret"));
        assert_eq!(link.calls.load(Ordering::SeqCst), 0);

        let files = fixture
            .registry
            .read_dir(&started.session_id, "/etc/telegraf")
            .await
            .expect("list");
        assert_eq!(files[0].name, "telegraf.conf");
        assert_eq!(link.calls.load(Ordering::SeqCst), 1);

        link.closed.store(true, Ordering::SeqCst);
        let closed = fixture
            .registry
            .read_dir(&started.session_id, "/etc")
            .await
            .expect_err("closed");
        assert!(matches!(closed, LinkError::NotConnected));
        assert_eq!(link.calls.load(Ordering::SeqCst), 1);
    }

    struct OpLink {
        closed: AtomicBool,
        nodes: Mutex<std::collections::HashMap<String, RawMeta>>,
        children: Mutex<std::collections::HashMap<String, Vec<String>>>,
        calls: Mutex<Vec<String>>,
    }

    impl OpLink {
        fn new() -> Self {
            Self {
                closed: AtomicBool::new(false),
                nodes: Mutex::new(std::collections::HashMap::new()),
                children: Mutex::new(std::collections::HashMap::new()),
                calls: Mutex::new(Vec::new()),
            }
        }

        fn note(&self, action: &str, path: &str) {
            self.calls
                .lock()
                .expect("calls")
                .push(format!("{action} {path}"));
        }

        fn put(&self, path: &str, meta: RawMeta) {
            self.nodes
                .lock()
                .expect("nodes")
                .insert(path.to_string(), meta);
        }
    }

    fn file_meta(is_dir: bool, is_symlink: bool) -> RawMeta {
        RawMeta {
            is_dir,
            is_file: !is_dir && !is_symlink,
            is_symlink,
            size: 4,
            modified: None,
        }
    }

    impl RemoteFs for OpLink {
        fn lstat_path<'a>(
            &'a self,
            path: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<RawMeta, LinkError>> + Send + 'a>,
        > {
            self.note("lstat", path);
            let result = self
                .nodes
                .lock()
                .expect("nodes")
                .get(path)
                .cloned()
                .ok_or(LinkError::NotFound);
            Box::pin(async move { result })
        }

        fn list_dir<'a>(
            &'a self,
            path: &'a str,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<Vec<RawFile>, LinkError>> + Send + 'a>,
        > {
            self.note("list", path);
            let names = self
                .children
                .lock()
                .expect("children")
                .get(path)
                .cloned()
                .unwrap_or_default();
            Box::pin(async move {
                Ok(names
                    .into_iter()
                    .map(|name| RawFile {
                        name,
                        meta: file_meta(false, false),
                    })
                    .collect())
            })
        }

        fn create_dir<'a>(
            &'a self,
            path: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), LinkError>> + Send + 'a>>
        {
            self.note("create_dir", path);
            Box::pin(async { Ok(()) })
        }

        fn rename_path<'a>(
            &'a self,
            from: &'a str,
            to: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), LinkError>> + Send + 'a>>
        {
            self.note("rename", from);
            self.note("rename_to", to);
            Box::pin(async { Ok(()) })
        }

        fn remove_file<'a>(
            &'a self,
            path: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), LinkError>> + Send + 'a>>
        {
            self.note("remove_file", path);
            Box::pin(async { Ok(()) })
        }

        fn remove_dir<'a>(
            &'a self,
            path: &'a str,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), LinkError>> + Send + 'a>>
        {
            self.note("remove_dir", path);
            Box::pin(async { Ok(()) })
        }
    }

    impl RemoteLink for OpLink {
        fn is_closed(&self) -> bool {
            self.closed.load(Ordering::SeqCst)
        }

        fn close(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
            self.closed.store(true, Ordering::SeqCst);
            Box::pin(async {})
        }
    }

    struct OpDialer {
        link: Arc<OpLink>,
    }

    impl Dialer for OpDialer {
        async fn dial(&self, _target: DialTarget) -> Result<Arc<dyn RemoteLink>, DialError> {
            Ok(self.link.clone())
        }
    }

    async fn connect_ops(link: Arc<OpLink>) -> (Fixture, String) {
        let fixture = fixture("password", None);
        save_password(&fixture, "hunter2-secret");
        let started = fixture
            .registry
            .connect(
                &OpDialer { link },
                &fixture.database,
                &fixture.vault,
                &fixture.hosts,
                &fixture.connection_id,
            )
            .await
            .expect("connect");
        (fixture, started.session_id)
    }

    #[tokio::test]
    async fn refused_paths_never_reach_the_link() {
        let registry = SessionRegistry::new();
        assert!(matches!(
            registry
                .remove_path("missing", "/")
                .await
                .expect_err("root"),
            LinkError::NotAllowed
        ));
        assert!(matches!(
            registry.create_dir("missing", "/").await.expect_err("root"),
            LinkError::NotAllowed
        ));
        assert!(matches!(
            registry
                .rename_path("missing", "/etc", "/etc/conf.d")
                .await
                .expect_err("into self"),
            LinkError::NotAllowed
        ));
        let nul = registry
            .remove_path("missing", "/etc/\0telegraf")
            .await
            .expect_err("nul");
        assert!(matches!(nul, LinkError::InvalidPath));
        assert!(!format!("{nul:?}").contains("hunter2-secret"));
        assert!(matches!(
            registry
                .rename_path("missing", "relative.conf", "/etc/other.conf")
                .await
                .expect_err("relative"),
            LinkError::InvalidPath
        ));
    }

    #[tokio::test]
    async fn delete_does_not_recurse_or_turn_a_path_into_a_command() {
        let link = Arc::new(OpLink::new());
        let awkward = "/etc/telegraf/my file; rm -rf.conf";
        link.put(awkward, file_meta(false, false));
        link.put("/etc/telegraf/link", file_meta(true, true));
        link.put("/etc/telegraf/conf.d", file_meta(true, false));
        link.put("/etc/telegraf/empty", file_meta(true, false));
        link.children.lock().expect("children").insert(
            "/etc/telegraf/conf.d".to_string(),
            vec![".".to_string(), "cpu.conf".to_string()],
        );
        link.children.lock().expect("children").insert(
            "/etc/telegraf/empty".to_string(),
            vec![".".to_string(), "..".to_string()],
        );
        let (fixture, session_id) = connect_ops(link.clone()).await;

        fixture
            .registry
            .remove_path(&session_id, awkward)
            .await
            .expect("file");
        fixture
            .registry
            .remove_path(&session_id, "/etc/telegraf/link")
            .await
            .expect("symlink");
        let occupied = fixture
            .registry
            .remove_path(&session_id, "/etc/telegraf/conf.d")
            .await
            .expect_err("occupied");
        assert!(matches!(occupied, LinkError::NotEmpty));
        fixture
            .registry
            .remove_path(&session_id, "/etc/telegraf/empty")
            .await
            .expect("empty");

        let calls = link.calls.lock().expect("calls").clone();
        assert_eq!(
            calls,
            vec![
                format!("lstat {awkward}"),
                format!("remove_file {awkward}"),
                "lstat /etc/telegraf/link".to_string(),
                "remove_file /etc/telegraf/link".to_string(),
                "lstat /etc/telegraf/conf.d".to_string(),
                "list /etc/telegraf/conf.d".to_string(),
                "lstat /etc/telegraf/empty".to_string(),
                "list /etc/telegraf/empty".to_string(),
                "remove_dir /etc/telegraf/empty".to_string(),
            ]
        );
        assert!(
            !calls
                .iter()
                .any(|call| call.contains("sudo") || call.contains("sh -c"))
        );
        assert!(!format!("{occupied:?}").contains("hunter2-secret"));
    }

    #[tokio::test]
    async fn mkdir_and_rename_do_not_overwrite_or_shell_out() {
        let link = Arc::new(OpLink::new());
        link.put("/etc/telegraf/cpu.conf", file_meta(false, false));
        let (fixture, session_id) = connect_ops(link.clone()).await;

        let exists = fixture
            .registry
            .create_dir(&session_id, "/etc/telegraf/cpu.conf")
            .await
            .expect_err("exists");
        assert!(matches!(exists, LinkError::AlreadyExists));

        fixture
            .registry
            .create_dir(&session_id, "/etc/telegraf/conf.d")
            .await
            .expect("mkdir");

        let same = fixture
            .registry
            .rename_path(
                &session_id,
                "/etc/telegraf/cpu.conf",
                "/etc/telegraf/cpu.conf",
            )
            .await;
        assert!(same.is_ok());

        let taken = fixture
            .registry
            .rename_path(
                &session_id,
                "/etc/telegraf/mem.conf",
                "/etc/telegraf/cpu.conf",
            )
            .await
            .expect_err("taken");
        assert!(matches!(taken, LinkError::AlreadyExists));

        fixture
            .registry
            .rename_path(
                &session_id,
                "/etc/telegraf/mem.conf",
                "/etc/telegraf/disk.conf",
            )
            .await
            .expect("rename");

        let calls = link.calls.lock().expect("calls").clone();
        assert_eq!(
            calls,
            vec![
                "lstat /etc/telegraf/cpu.conf".to_string(),
                "lstat /etc/telegraf/conf.d".to_string(),
                "create_dir /etc/telegraf/conf.d".to_string(),
                "lstat /etc/telegraf/cpu.conf".to_string(),
                "lstat /etc/telegraf/disk.conf".to_string(),
                "rename /etc/telegraf/mem.conf".to_string(),
                "rename_to /etc/telegraf/disk.conf".to_string(),
            ]
        );
        assert!(!calls.iter().any(|call| call.contains("sudo")));
    }
}
