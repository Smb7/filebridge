use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client;
use russh::keys::{PrivateKeyWithHashAlg, decode_secret_key};
use russh_sftp::client::SftpSession;
use zeroize::Zeroizing;

use super::sftp::SftpGate;
use super::{DialError, DialTarget, Dialer, PreparedAuth, RemoteLink, matches_trusted_key};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
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
        let handler = KeyHandler {
            known: target.known_keys,
            rejected: rejected.clone(),
        };
        let config = Arc::new(client::Config {
            inactivity_timeout: None,
            nodelay: true,
            ..client::Config::default()
        });
        let connect = client::connect(config, (target.host.as_str(), target.port), handler);
        let mut handle = match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
            Ok(Ok(handle)) => handle,
            Ok(Err(_)) | Err(_) => return Err(take_rejection(&rejected)),
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
        if let Err(error) = link.probe_root().await {
            link.close().await;
            return Err(error);
        }
        Ok(link)
    }
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
    async fn probe_root(&self) -> Result<(), DialError> {
        self.gate
            .run("/", |path| async move {
                let sftp = self.sftp.lock().await;
                sftp.read_dir(&path).await.map(|_| ()).map_err(|_| ())
            })
            .await
            .map_err(|_| DialError::Sftp)?
            .map_err(|_| DialError::Sftp)
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
    fn garbage_key_material_is_an_auth_failure_without_echoing_it() {
        let secret = b"not-a-key hunter2-secret";
        let error = decode_identity(secret, Some("hunter2-secret")).expect_err("garbage");
        let message = format!("{error:?}");
        assert!(!message.contains("hunter2-secret"));
    }
}
