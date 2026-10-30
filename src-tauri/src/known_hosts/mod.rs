use std::collections::HashMap;
use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD as BASE64;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::db::{Database, DbError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostKeyDecision {
    Trusted,
    Unknown,
    Changed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKeySummary {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub key_type: String,
    pub fingerprint: String,
    pub trusted_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKeyPrompt {
    pub pending_id: String,
    pub host: String,
    pub port: u16,
    pub key_type: String,
    pub fingerprint: String,
    pub stored_fingerprint: Option<String>,
    pub decision: HostKeyDecision,
}

#[derive(Debug, Serialize)]
pub struct CommandError {
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug)]
pub(crate) enum HostKeyError {
    Unknown,
    Changed,
    Invalid(&'static str),
    NotFound,
    Database,
}

impl From<HostKeyError> for CommandError {
    fn from(error: HostKeyError) -> Self {
        let (code, message) = match error {
            HostKeyError::Unknown => (
                "host_key_unknown",
                "This host key has not been trusted yet.",
            ),
            HostKeyError::Changed => (
                "host_key_changed",
                "The host key changed. Trust was not replaced.",
            ),
            HostKeyError::Invalid(message) => ("invalid_host_key", message),
            HostKeyError::NotFound => ("not_found", "Trusted host was not found."),
            HostKeyError::Database => ("database", "Could not update trusted hosts."),
        };
        Self {
            code,
            message: message.to_string(),
        }
    }
}

impl From<rusqlite::Error> for HostKeyError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Database
    }
}

impl From<DbError> for CommandError {
    fn from(_: DbError) -> Self {
        Self {
            code: "database",
            message: "Could not update trusted hosts.".to_string(),
        }
    }
}

struct PresentedKey {
    host: String,
    port: u16,
    key_type: String,
    public_key: Vec<u8>,
}

struct StoredKey {
    id: String,
    fingerprint: String,
    public_key: Vec<u8>,
    trusted_at: String,
}

struct Pending {
    presented: PresentedKey,
    decision: HostKeyDecision,
    fingerprint: String,
    stored_fingerprint: Option<String>,
}

pub struct HostKeyState {
    pending: Mutex<HashMap<String, Pending>>,
}

impl HostKeyState {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn present(
        &self,
        connection: &Connection,
        host: &str,
        port: u16,
        key_type: &str,
        public_key: &[u8],
    ) -> Result<HostKeyPrompt, HostKeyError> {
        let presented = validate_presented(host, port, key_type, public_key)?;
        let fingerprint = fingerprint(&presented.public_key);
        let stored = find_key(connection, &presented)?;
        let decision = classify(
            stored.as_ref().map(|key| key.public_key.as_slice()),
            public_key,
        );
        let stored_fingerprint = stored.as_ref().map(|key| key.fingerprint.clone());
        let pending_id = uuid::Uuid::new_v4().to_string();
        if decision != HostKeyDecision::Trusted {
            let mut pending = self.pending.lock().map_err(|_| HostKeyError::Database)?;
            pending.insert(
                pending_id.clone(),
                Pending {
                    presented,
                    decision,
                    fingerprint: fingerprint.clone(),
                    stored_fingerprint: stored_fingerprint.clone(),
                },
            );
        }
        Ok(HostKeyPrompt {
            pending_id,
            host: host.trim().to_ascii_lowercase(),
            port,
            key_type: key_type.trim().to_string(),
            fingerprint,
            stored_fingerprint,
            decision,
        })
    }

    fn confirm_on(
        &self,
        connection: &Connection,
        pending_id: &str,
        replace: bool,
    ) -> Result<HostKeySummary, HostKeyError> {
        let pending = self
            .pending
            .lock()
            .map_err(|_| HostKeyError::Database)?
            .get(pending_id)
            .map(|pending| Pending {
                presented: PresentedKey {
                    host: pending.presented.host.clone(),
                    port: pending.presented.port,
                    key_type: pending.presented.key_type.clone(),
                    public_key: pending.presented.public_key.clone(),
                },
                decision: pending.decision,
                fingerprint: pending.fingerprint.clone(),
                stored_fingerprint: pending.stored_fingerprint.clone(),
            })
            .ok_or(HostKeyError::NotFound)?;

        let saved = match (pending.decision, replace) {
            (HostKeyDecision::Unknown, false) => trust(connection, &pending.presented)?,
            (HostKeyDecision::Changed, true) => replace_key(connection, &pending.presented)?,
            (HostKeyDecision::Changed, false) => return Err(HostKeyError::Changed),
            (HostKeyDecision::Unknown, true) => return Err(HostKeyError::Unknown),
            (HostKeyDecision::Trusted, _) => return Err(HostKeyError::NotFound),
        };
        let _ = self
            .pending
            .lock()
            .map_err(|_| HostKeyError::Database)?
            .remove(pending_id);
        Ok(saved)
    }

    /// Trust only the key the user was shown. A changed key is replaced only
    /// when the fingerprint matches that presented key.
    pub(crate) fn confirm_fingerprint(
        &self,
        connection: &Connection,
        pending_id: &str,
        fingerprint: &str,
    ) -> Result<HostKeySummary, HostKeyError> {
        let (pending_fingerprint, replace) = {
            let pending = self.pending.lock().map_err(|_| HostKeyError::Database)?;
            let pending = pending.get(pending_id).ok_or(HostKeyError::NotFound)?;
            (
                pending.fingerprint.clone(),
                pending.decision == HostKeyDecision::Changed,
            )
        };
        if pending_fingerprint != fingerprint {
            return Err(HostKeyError::Invalid(
                "The fingerprint does not match the presented host key.",
            ));
        }
        self.confirm_on(connection, pending_id, replace)
    }
}

impl Default for HostKeyState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn fingerprint(public_key: &[u8]) -> String {
    let digest = Sha256::digest(public_key);
    format!("SHA256:{}", BASE64.encode(digest))
}

pub(crate) struct StoredHostKey {
    pub key_type: String,
    pub public_key: Vec<u8>,
}

pub(crate) fn decision_for(stored: Option<&[u8]>, presented: &[u8]) -> HostKeyDecision {
    classify(stored, presented)
}

pub(crate) fn stored_keys(
    connection: &Connection,
    host: &str,
    port: u16,
) -> Result<Vec<StoredHostKey>, HostKeyError> {
    let host = host.trim().to_ascii_lowercase();
    let mut statement = connection
        .prepare("SELECT key_type, public_key FROM known_hosts WHERE host = ?1 AND port = ?2")?;
    let rows = statement.query_map(params![host, port], |row| {
        Ok(StoredHostKey {
            key_type: row.get(0)?,
            public_key: row.get(1)?,
        })
    })?;
    let mut keys = Vec::new();
    for row in rows {
        keys.push(row?);
    }
    Ok(keys)
}

fn classify(stored: Option<&[u8]>, presented: &[u8]) -> HostKeyDecision {
    match stored {
        None => HostKeyDecision::Unknown,
        Some(stored) if stored == presented => HostKeyDecision::Trusted,
        Some(_) => HostKeyDecision::Changed,
    }
}

fn trust(
    connection: &Connection,
    presented: &PresentedKey,
) -> Result<HostKeySummary, HostKeyError> {
    match find_key(connection, presented)? {
        Some(stored) if stored.public_key == presented.public_key => {
            Ok(summary_from(presented, &stored))
        }
        Some(_) => Err(HostKeyError::Changed),
        None => insert_key(connection, presented),
    }
}

fn replace_key(
    connection: &Connection,
    presented: &PresentedKey,
) -> Result<HostKeySummary, HostKeyError> {
    match find_key(connection, presented)? {
        Some(stored) if stored.public_key != presented.public_key => {
            let fingerprint = fingerprint(&presented.public_key);
            let changed = connection.execute(
                "UPDATE known_hosts
                 SET fingerprint = ?1, public_key = ?2, trusted_at = datetime('now')
                 WHERE id = ?3",
                params![fingerprint, presented.public_key, stored.id],
            )?;
            if changed != 1 {
                return Err(HostKeyError::Database);
            }
            let updated = find_key(connection, presented)?.ok_or(HostKeyError::Database)?;
            Ok(summary_from(presented, &updated))
        }
        Some(_) => Err(HostKeyError::Unknown),
        None => Err(HostKeyError::Unknown),
    }
}

fn insert_key(
    connection: &Connection,
    presented: &PresentedKey,
) -> Result<HostKeySummary, HostKeyError> {
    let id = uuid::Uuid::new_v4().to_string();
    let fingerprint = fingerprint(&presented.public_key);
    connection.execute(
        "INSERT INTO known_hosts (id, host, port, key_type, fingerprint, public_key, trusted_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, datetime('now'))",
        params![
            id,
            presented.host,
            presented.port,
            presented.key_type,
            fingerprint,
            presented.public_key,
        ],
    )?;
    let stored = find_key(connection, presented)?.ok_or(HostKeyError::Database)?;
    Ok(summary_from(presented, &stored))
}

fn summary_from(presented: &PresentedKey, stored: &StoredKey) -> HostKeySummary {
    HostKeySummary {
        id: stored.id.clone(),
        host: presented.host.clone(),
        port: presented.port,
        key_type: presented.key_type.clone(),
        fingerprint: stored.fingerprint.clone(),
        trusted_at: stored.trusted_at.clone(),
    }
}

fn find_key(
    connection: &Connection,
    presented: &PresentedKey,
) -> Result<Option<StoredKey>, HostKeyError> {
    let row = connection
        .query_row(
            "SELECT id, fingerprint, public_key, trusted_at
             FROM known_hosts
             WHERE host = ?1 AND port = ?2 AND key_type = ?3",
            params![presented.host, presented.port, presented.key_type],
            |row| {
                Ok(StoredKey {
                    id: row.get(0)?,
                    fingerprint: row.get(1)?,
                    public_key: row.get(2)?,
                    trusted_at: row.get(3)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

fn list(connection: &Connection) -> Result<Vec<HostKeySummary>, HostKeyError> {
    let mut statement = connection.prepare(
        "SELECT id, host, port, key_type, fingerprint, trusted_at
         FROM known_hosts
         ORDER BY host, port, key_type",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(HostKeySummary {
            id: row.get(0)?,
            host: row.get(1)?,
            port: row.get(2)?,
            key_type: row.get(3)?,
            fingerprint: row.get(4)?,
            trusted_at: row.get(5)?,
        })
    })?;
    let mut hosts = Vec::new();
    for row in rows {
        hosts.push(row?);
    }
    Ok(hosts)
}

fn forget(connection: &Connection, id: &str) -> Result<(), HostKeyError> {
    let changed = connection.execute("DELETE FROM known_hosts WHERE id = ?1", [id])?;
    if changed == 0 {
        Err(HostKeyError::NotFound)
    } else {
        Ok(())
    }
}

fn validate_presented(
    host: &str,
    port: u16,
    key_type: &str,
    public_key: &[u8],
) -> Result<PresentedKey, HostKeyError> {
    let host = host.trim().to_ascii_lowercase();
    let key_type = key_type.trim();
    if host.is_empty()
        || host.chars().count() > 253
        || host.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(HostKeyError::Invalid("Host is not valid."));
    }
    if port == 0 {
        return Err(HostKeyError::Invalid("Port must be between 1 and 65535."));
    }
    if key_type.is_empty()
        || key_type.chars().count() > 64
        || key_type
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(HostKeyError::Invalid("Host key type is not valid."));
    }
    if public_key.is_empty() || public_key.len() > 16_384 {
        return Err(HostKeyError::Invalid("Host key is not valid."));
    }
    Ok(PresentedKey {
        host,
        port,
        key_type: key_type.to_string(),
        public_key: public_key.to_vec(),
    })
}

#[tauri::command]
pub fn hostkey_present(
    database: tauri::State<'_, Database>,
    state: tauri::State<'_, HostKeyState>,
    host: String,
    port: u16,
    key_type: String,
    public_key: Vec<u8>,
) -> Result<HostKeyPrompt, CommandError> {
    database.with_connection(|connection| {
        state
            .present(connection, &host, port, &key_type, &public_key)
            .map_err(CommandError::from)
    })
}

#[tauri::command]
pub fn hostkey_list(
    database: tauri::State<'_, Database>,
) -> Result<Vec<HostKeySummary>, CommandError> {
    database.with_connection(|connection| list(connection).map_err(CommandError::from))
}

#[tauri::command]
pub fn hostkey_forget(
    database: tauri::State<'_, Database>,
    id: String,
) -> Result<(), CommandError> {
    database.with_connection(|connection| forget(connection, &id).map_err(CommandError::from))
}

#[tauri::command]
pub fn hostkey_confirm(
    database: tauri::State<'_, Database>,
    state: tauri::State<'_, HostKeyState>,
    pending_id: String,
    replace: bool,
) -> Result<HostKeySummary, CommandError> {
    database.with_connection(|connection| {
        state
            .confirm_on(connection, &pending_id, replace)
            .map_err(CommandError::from)
    })
}

#[cfg(test)]
mod tests {
    use super::{
        HostKeyDecision, HostKeyState, classify, fingerprint, forget, list, replace_key,
        stored_keys, trust,
    };
    use crate::db::apply_migrations;
    use rusqlite::Connection;

    fn open_db() -> Connection {
        let mut connection = Connection::open_in_memory().expect("memory database");
        apply_migrations(&mut connection).expect("migrate");
        connection
    }

    fn presented(key: &[u8]) -> super::PresentedKey {
        super::validate_presented("Example.COM", 22, "ssh-ed25519", key).expect("presented")
    }

    #[test]
    fn fingerprint_is_sha256_without_padding() {
        let value = fingerprint(b"test-key");
        assert!(value.starts_with("SHA256:"));
        assert!(!value.ends_with('='));
        assert_ne!(fingerprint(b"test-key"), fingerprint(b"other-key"));
    }

    #[test]
    fn classify_distinguishes_unknown_trusted_and_changed() {
        assert_eq!(classify(None, b"key-a"), HostKeyDecision::Unknown);
        assert_eq!(classify(Some(b"key-a"), b"key-a"), HostKeyDecision::Trusted);
        assert_eq!(classify(Some(b"key-a"), b"key-b"), HostKeyDecision::Changed);
    }

    #[test]
    fn trust_does_not_replace_a_changed_key() {
        let connection = open_db();
        let original = presented(b"key-a");
        let trusted = trust(&connection, &original).expect("first trust");
        let before = list(&connection).expect("list");

        let changed = presented(b"key-b");
        let error = trust(&connection, &changed).expect_err("changed");
        let after = list(&connection).expect("list after refusal");

        assert!(matches!(error, super::HostKeyError::Changed));
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].fingerprint, before[0].fingerprint);
        assert_eq!(after[0].id, trusted.id);
        assert_eq!(after[0].trusted_at, trusted.trusted_at);
    }

    #[test]
    fn replace_is_explicit_and_does_not_create_an_unknown_key() {
        let connection = open_db();
        let original = presented(b"key-a");
        let error = replace_key(&connection, &original).expect_err("no stored key");
        assert!(matches!(error, super::HostKeyError::Unknown));
        assert!(list(&connection).expect("empty").is_empty());

        trust(&connection, &original).expect("trust");
        let changed = presented(b"key-b");
        let replaced = replace_key(&connection, &changed).expect("replace");
        let hosts = list(&connection).expect("list");
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].fingerprint, replaced.fingerprint);
        assert_ne!(hosts[0].fingerprint, fingerprint(b"key-a"));
        assert_eq!(hosts[0].host, "example.com");
    }

    #[test]
    fn confirm_refuses_to_replace_when_the_prompt_is_only_trust() {
        let connection = open_db();
        let state = HostKeyState::new();
        trust(&connection, &presented(b"key-a")).expect("trust");
        let prompt = state
            .present(&connection, "example.com", 22, "ssh-ed25519", b"key-b")
            .expect("present");
        assert_eq!(prompt.decision, HostKeyDecision::Changed);

        let error = state
            .confirm_on(&connection, &prompt.pending_id, false)
            .expect_err("trust must not replace");
        assert!(matches!(error, super::HostKeyError::Changed));
        assert_eq!(list(&connection).expect("list").len(), 1);
        assert_eq!(
            list(&connection).expect("list")[0].fingerprint,
            fingerprint(b"key-a")
        );

        forget(&connection, &list(&connection).expect("list")[0].id).expect("forget");
        assert!(list(&connection).expect("empty").is_empty());
    }

    #[test]
    fn confirm_fingerprint_matches_the_presented_key_and_does_not_silently_replace() {
        let connection = open_db();
        let state = HostKeyState::new();
        let unknown = state
            .present(&connection, "example.com", 22, "ssh-ed25519", b"key-a")
            .expect("present unknown");
        let mismatch = state
            .confirm_fingerprint(&connection, &unknown.pending_id, "SHA256:not-the-key")
            .expect_err("mismatch");
        assert!(matches!(mismatch, super::HostKeyError::Invalid(_)));
        assert!(list(&connection).expect("still empty").is_empty());

        state
            .confirm_fingerprint(&connection, &unknown.pending_id, &unknown.fingerprint)
            .expect("trust");
        assert_eq!(list(&connection).expect("trusted").len(), 1);

        let changed = state
            .present(&connection, "example.com", 22, "ssh-ed25519", b"key-b")
            .expect("present changed");
        assert_eq!(changed.decision, HostKeyDecision::Changed);
        let refused = state
            .confirm_fingerprint(&connection, &changed.pending_id, &fingerprint(b"key-a"))
            .expect_err("old fingerprint must not replace");
        assert!(matches!(refused, super::HostKeyError::Invalid(_)));
        assert_eq!(
            list(&connection).expect("unchanged")[0].fingerprint,
            fingerprint(b"key-a")
        );

        state
            .confirm_fingerprint(&connection, &changed.pending_id, &changed.fingerprint)
            .expect("explicit replace");
        assert_eq!(
            list(&connection).expect("replaced")[0].fingerprint,
            fingerprint(b"key-b")
        );
        let keys = stored_keys(&connection, "Example.COM", 22).expect("stored");
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].key_type, "ssh-ed25519");
        assert_eq!(keys[0].public_key, b"key-b");
    }
}
