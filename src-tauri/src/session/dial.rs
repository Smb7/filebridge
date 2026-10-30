use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client;
use russh::keys::{PrivateKeyWithHashAlg, decode_secret_key};
use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileAttributes, StatusCode};
use zeroize::Zeroizing;

use super::sftp::SftpGate;
use super::{
    DialError, DialTarget, Dialer, LinkError, PreparedAuth, RawFile, RawMeta, RemoteFs, RemoteLink,
    matches_trusted_key,
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(30);
const SFTP_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_KEY_BYTES: u64 = 256 * 1024;

enum Rejection {
    Untrusted {
        key_type: String,
        public_key: Vec<u8>,
    },
    Certificate,
}

struct KeyHandler {
    known: Vec<crate::known_hosts::StoredHostKey>,
    rejected: Arc<Mutex<Option<Rejection>>>,
}

impl client::Handler for KeyHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        if server_public_key.certificate().is_some() {
            if let Ok(mut slot) = self.rejected.lock() {
                *slot = Some(Rejection::Certificate);
            }
            return Ok(false);
        }
        let public = server_public_key.public_key();
        let key_type = public.algorithm().as_str().to_string();
        let Ok(public_key) = public.to_bytes() else {
            return Ok(false);
        };
        if matches_trusted_key(&self.known, &key_type, &public_key) {
            return Ok(true);
        }
        if let Ok(mut slot) = self.rejected.lock() {
            *slot = Some(Rejection::Untrusted {
                key_type,
                public_key,
            });
        }
        Ok(false)
    }
}

pub struct RusshDialer;

impl Dialer for RusshDialer {
    async fn dial(&self, target: DialTarget) -> Result<Arc<dyn RemoteLink>, DialError> {
        let rejected = Arc::new(Mutex::new(None));
        let rejected_for_timeout = rejected.clone();
        // The handshake, authentication, and SFTP startup share one deadline.
        // A server that accepts TCP and then stalls must not hold the session lock.
        match tokio::time::timeout(CONNECT_TIMEOUT, open_link(target, rejected)).await {
            Ok(result) => result,
            Err(_) => Err(take_rejection(&rejected_for_timeout)),
        }
    }
}

async fn open_link(
    target: DialTarget,
    rejected: Arc<Mutex<Option<Rejection>>>,
) -> Result<Arc<dyn RemoteLink>, DialError> {
    let handler = KeyHandler {
        known: target.known_keys,
        rejected: rejected.clone(),
    };
    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(KEEPALIVE_INTERVAL),
        keepalive_max: 3,
        nodelay: true,
        ..client::Config::default()
    });
    let mut handle =
        match client::connect(config, (target.host.as_str(), target.port), handler).await {
            Ok(handle) => handle,
            Err(_) => return Err(take_rejection(&rejected)),
        };

    if let Err(error) = authenticate(&mut handle, &target.username, target.auth).await {
        let _ = handle
            .disconnect(russh::Disconnect::ByApplication, "", "")
            .await;
        return Err(error);
    }

    let channel = match handle.channel_open_session().await {
        Ok(channel) => channel,
        Err(_) => {
            let _ = handle
                .disconnect(russh::Disconnect::ByApplication, "", "")
                .await;
            return Err(DialError::Sftp);
        }
    };
    if channel.request_subsystem(true, "sftp").await.is_err() {
        let _ = handle
            .disconnect(russh::Disconnect::ByApplication, "", "")
            .await;
        return Err(DialError::Sftp);
    }
    let sftp = match SftpSession::new(channel.into_stream()).await {
        Ok(sftp) => sftp,
        Err(_) => {
            let _ = handle
                .disconnect(russh::Disconnect::ByApplication, "", "")
                .await;
            return Err(DialError::Sftp);
        }
    };
    let link = Arc::new(RusshLink {
        handle: tokio::sync::Mutex::new(handle),
        sftp: tokio::sync::Mutex::new(sftp),
        gate: SftpGate::new(),
        closed: std::sync::atomic::AtomicBool::new(false),
    });
    // Listing "/" fails for chrooted and restricted accounts, so a refused
    // realpath does not fail the connection. A hang still means SFTP is unusable.
    if tokio::time::timeout(SFTP_PROBE_TIMEOUT, probe_sftp(&link.sftp, &link.gate))
        .await
        .is_err()
    {
        link.close().await;
        return Err(DialError::Sftp);
    }
    Ok(link)
}

async fn probe_sftp(sftp: &tokio::sync::Mutex<SftpSession>, gate: &SftpGate) {
    let _ = gate
        .run("/", |path| async move {
            let sftp = sftp.lock().await;
            let _ = sftp.canonicalize(&path).await;
        })
        .await;
}

fn take_rejection(rejected: &Mutex<Option<Rejection>>) -> DialError {
    match rejected.lock().ok().and_then(|mut slot| slot.take()) {
        Some(Rejection::Untrusted {
            key_type,
            public_key,
        }) => DialError::Presented {
            key_type,
            public_key,
        },
        Some(Rejection::Certificate) => DialError::Certificate,
        None => DialError::Unreachable,
    }
}

async fn authenticate(
    handle: &mut client::Handle<KeyHandler>,
    username: &str,
    auth: PreparedAuth,
) -> Result<(), DialError> {
    let success = match auth {
        PreparedAuth::Password(password) => handle
            .authenticate_password(username, password.as_str())
            .await
            .map_err(|_| DialError::AuthFailed)?
            .success(),
        PreparedAuth::PrivateKey { pem, passphrase } => {
            let key = decode_identity(pem.as_bytes(), passphrase.as_deref().map(String::as_str))?;
            finish_public_key(handle, username, key).await?
        }
        PreparedAuth::KeyFile { path, passphrase } => {
            let key = read_key_file(&path, passphrase.as_deref().map(String::as_str)).await?;
            finish_public_key(handle, username, key).await?
        }
    };
    if success {
        Ok(())
    } else {
        Err(DialError::AuthFailed)
    }
}

/// `path` is the saved local key file. Remote paths never reach `tokio::fs`.
async fn read_key_file(
    path: &str,
    passphrase: Option<&str>,
) -> Result<russh::keys::PrivateKey, DialError> {
    if path.is_empty() || path.contains('\0') {
        return Err(DialError::AuthFailed);
    }
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|_| DialError::AuthFailed)?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_KEY_BYTES {
        return Err(DialError::AuthFailed);
    }
    let bytes = Zeroizing::new(
        tokio::fs::read(path)
            .await
            .map_err(|_| DialError::AuthFailed)?,
    );
    decode_identity(&bytes, passphrase)
}

fn decode_identity(
    bytes: &[u8],
    passphrase: Option<&str>,
) -> Result<russh::keys::PrivateKey, DialError> {
    let text = std::str::from_utf8(bytes).map_err(|_| DialError::AuthFailed)?;
    decode_secret_key(text, passphrase).map_err(|_| DialError::AuthFailed)
}

async fn finish_public_key(
    handle: &mut client::Handle<KeyHandler>,
    username: &str,
    key: russh::keys::PrivateKey,
) -> Result<bool, DialError> {
    let hash_alg = if key.algorithm().is_rsa() {
        handle
            .best_supported_rsa_hash()
            .await
            .ok()
            .flatten()
            .flatten()
    } else {
        None
    };
    let wrapped = PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg);
    handle
        .authenticate_publickey(username, wrapped)
        .await
        .map(|result| result.success())
        .map_err(|_| DialError::AuthFailed)
}

struct RusshLink {
    handle: tokio::sync::Mutex<client::Handle<KeyHandler>>,
    sftp: tokio::sync::Mutex<SftpSession>,
    gate: SftpGate,
    closed: std::sync::atomic::AtomicBool,
}

impl RusshLink {
    async fn read_remote_dir(&self, path: &str) -> Result<Vec<RawFile>, LinkError> {
        let sftp = &self.sftp;
        match self
            .gate
            .run(path, |absolute| async move {
                let session = sftp.lock().await;
                collect_dir(&session, &absolute).await
            })
            .await
        {
            Ok(result) => result,
            Err(_) => Err(LinkError::InvalidPath),
        }
    }

    async fn read_remote_stat(&self, path: &str) -> Result<RawMeta, LinkError> {
        let sftp = &self.sftp;
        match self
            .gate
            .run(path, |absolute| async move {
                let session = sftp.lock().await;
                collect_stat(&session, &absolute).await
            })
            .await
        {
            Ok(result) => result,
            Err(_) => Err(LinkError::InvalidPath),
        }
    }
}

impl RemoteFs for RusshLink {
    fn list_dir<'a>(
        &'a self,
        path: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<RawFile>, LinkError>> + Send + 'a>,
    > {
        Box::pin(self.read_remote_dir(path))
    }

    fn stat_path<'a>(
        &'a self,
        path: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<RawMeta, LinkError>> + Send + 'a>>
    {
        Box::pin(self.read_remote_stat(path))
    }
}

fn meta_from(metadata: &FileAttributes) -> RawMeta {
    RawMeta {
        is_dir: metadata.is_dir(),
        is_file: metadata.is_regular(),
        is_symlink: metadata.is_symlink(),
        size: metadata.len(),
        modified: metadata.mtime.map(i64::from),
    }
}

async fn collect_dir(sftp: &SftpSession, path: &str) -> Result<Vec<RawFile>, LinkError> {
    let listing = sftp.read_dir(path).await.map_err(map_client_error)?;
    let mut files = Vec::new();
    for entry in listing {
        let metadata = entry.metadata();
        files.push(RawFile {
            name: entry.file_name(),
            meta: meta_from(&metadata),
        });
    }
    Ok(files)
}

async fn collect_stat(sftp: &SftpSession, path: &str) -> Result<RawMeta, LinkError> {
    let metadata = sftp.metadata(path).await.map_err(map_client_error)?;
    Ok(meta_from(&metadata))
}

pub(crate) fn link_error_from_code(code: StatusCode) -> LinkError {
    match code {
        StatusCode::NoSuchFile => LinkError::NotFound,
        StatusCode::PermissionDenied => LinkError::PermissionDenied,
        StatusCode::NoConnection | StatusCode::ConnectionLost => LinkError::NotConnected,
        _ => LinkError::Failed,
    }
}

fn map_client_error(error: russh_sftp::client::error::Error) -> LinkError {
    match error {
        russh_sftp::client::error::Error::Status(status) => {
            link_error_from_code(status.status_code)
        }
        _ => LinkError::Failed,
    }
}

impl RemoteLink for RusshLink {
    fn is_closed(&self) -> bool {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return true;
        }
        self.handle
            .try_lock()
            .map(|handle| handle.is_closed())
            .unwrap_or(false)
    }

    fn close(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
            let handle = self.handle.lock().await;
            let _ = handle
                .disconnect(russh::Disconnect::ByApplication, "", "")
                .await;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::decode_identity;

    #[test]
    fn a_server_status_message_is_not_part_of_the_link_error() {
        let error = russh_sftp::client::error::Error::Status(russh_sftp::protocol::Status {
            id: 1,
            status_code: russh_sftp::protocol::StatusCode::PermissionDenied,
            error_message: "hunter2-secret".to_string(),
            language_tag: String::new(),
        });
        let mapped = super::map_client_error(error);
        assert!(matches!(mapped, super::LinkError::PermissionDenied));
        assert!(!format!("{mapped:?}").contains("hunter2-secret"));
        assert!(matches!(
            super::link_error_from_code(russh_sftp::protocol::StatusCode::NoSuchFile),
            super::LinkError::NotFound
        ));
    }

    #[test]
    fn garbage_key_material_is_an_auth_failure_without_echoing_it() {
        let secret = b"not-a-key hunter2-secret";
        let error = decode_identity(secret, Some("hunter2-secret")).expect_err("garbage");
        let message = format!("{error:?}");
        assert!(!message.contains("hunter2-secret"));
    }

    #[test]
    fn stored_host_key_fingerprint_matches_openssh() {
        let public = russh::keys::parse_public_key_base64(
            "AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl",
        )
        .expect("public key");
        let bytes = public.to_bytes().expect("wire bytes");
        let openssh = public.fingerprint(russh::keys::HashAlg::Sha256).to_string();
        assert_eq!(public.algorithm().as_str(), "ssh-ed25519");
        assert_eq!(crate::known_hosts::fingerprint(&bytes), openssh);
        assert!(openssh.starts_with("SHA256:"));
        assert!(!openssh.contains("hunter2-secret"));
    }
}
