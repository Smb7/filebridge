mod crypto;
mod keystore;

use std::sync::Mutex;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use crate::connections::AuthMethod;
use crate::db::{Database, DbError};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::vault::crypto::{KdfParams, WrappedDek, minimum_kdf, production_kdf, test_kdf};
use crate::vault::keystore::{KeyStore, KeyStoreError, MemoryKeyStore, OsKeyStore};

const MODE_KEY: &str = "vault_mode";
const WRAPPED_KEY: &str = "wrapped_dek";
const SALT_KEY: &str = "kdf_salt";
const PARAMS_KEY: &str = "kdf_params";
const FAILURES_KEY: &str = "vault_unlock_failures";
const LOCKED_UNTIL_KEY: &str = "vault_unlock_locked_until";
const LOCKOUT_AFTER: u64 = 5;
const MAX_LOCKOUT_SECS: u64 = 900;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VaultMode {
    Uninitialized,
    Keychain,
    Password,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultStatus {
    pub mode: VaultMode,
    pub locked: bool,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSecret {
    pub password: Option<String>,
    pub private_key_pem: Option<String>,
    pub passphrase: Option<String>,
}

impl std::fmt::Debug for CredentialSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CredentialSecret([redacted])")
    }
}

impl Drop for CredentialSecret {
    fn drop(&mut self) {
        if let Some(value) = &mut self.password {
            value.zeroize();
        }
        if let Some(value) = &mut self.private_key_pem {
            value.zeroize();
        }
        if let Some(value) = &mut self.passphrase {
            value.zeroize();
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CommandError {
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug)]
enum VaultError {
    Locked,
    PasswordRequired,
    InvalidPassword,
    LockedOut,
    AlreadyInitialized,
    KeychainAvailable,
    Keychain,
    InvalidCredential(&'static str),
    NotFound,
    Database,
}

impl From<VaultError> for CommandError {
    fn from(error: VaultError) -> Self {
        let (code, message) = match error {
            VaultError::Locked => ("vault_locked", "Unlock the vault first."),
            VaultError::PasswordRequired => ("password_required", "A master password is required."),
            VaultError::InvalidPassword => ("invalid_password", "Incorrect password."),
            VaultError::LockedOut => ("locked_out", "Too many unlock attempts. Try again later."),
            VaultError::AlreadyInitialized => {
                ("already_initialized", "The vault is already set up.")
            }
            VaultError::KeychainAvailable => (
                "keychain_available",
                "The system keychain is available. A master password is not used.",
            ),
            VaultError::Keychain => ("keychain", "Could not unlock the system keychain."),
            VaultError::InvalidCredential(message) => ("invalid_credential", message),
            VaultError::NotFound => ("not_found", "Connection not found."),
            VaultError::Database => ("database", "Could not update the vault."),
        };
        Self {
            code,
            message: message.to_string(),
        }
    }
}

impl From<DbError> for CommandError {
    fn from(_: DbError) -> Self {
        Self {
            code: "database",
            message: "Could not update the vault.".to_string(),
        }
    }
}

impl From<DbError> for VaultError {
    fn from(_: DbError) -> Self {
        Self::Database
    }
}

impl From<rusqlite::Error> for VaultError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Database
    }
}

impl From<KeyStoreError> for VaultError {
    fn from(_: KeyStoreError) -> Self {
        Self::Keychain
    }
}

pub struct VaultState {
    keystore: Box<dyn KeyStore>,
    dek: Mutex<Option<Zeroizing<Vec<u8>>>>,
    kdf: KdfParams,
}

impl VaultState {
    pub fn system() -> Self {
        Self::new(Box::new(OsKeyStore), production_kdf())
    }

    fn new(keystore: Box<dyn KeyStore>, kdf: KdfParams) -> Self {
        Self {
            keystore,
            dek: Mutex::new(None),
            kdf,
        }
    }

    fn status_on(&self, connection: &Connection) -> Result<VaultStatus, VaultError> {
        let mode = stored_mode(connection)?;
        let locked = self.dek.lock().map_err(|_| VaultError::Database)?.is_none();
        Ok(VaultStatus { mode, locked })
    }

    fn init_password_on(&self, connection: &Connection, password: &str) -> Result<(), VaultError> {
        if self.keystore.available() {
            return Err(VaultError::KeychainAvailable);
        }
        if password.chars().count() < 8 {
            return Err(VaultError::InvalidCredential(
                "Master password must be at least 8 characters.",
            ));
        }
        if stored_mode(connection)? != VaultMode::Uninitialized {
            return Err(VaultError::AlreadyInitialized);
        }
        let dek = crypto::random_dek().map_err(|_| VaultError::Database)?;
        let wrapped =
            crypto::wrap_dek(password, &self.kdf, &dek).map_err(|_| VaultError::Database)?;
        let transaction = connection.unchecked_transaction()?;
        if stored_mode(&transaction)? != VaultMode::Uninitialized {
            return Err(VaultError::AlreadyInitialized);
        }
        set_meta(&transaction, MODE_KEY, "password")?;
        set_meta(&transaction, SALT_KEY, &BASE64.encode(&wrapped.salt))?;
        set_meta(
            &transaction,
            WRAPPED_KEY,
            &BASE64.encode(join_nonce(&wrapped.nonce, &wrapped.ciphertext)),
        )?;
        set_meta(
            &transaction,
            PARAMS_KEY,
            &serde_json::to_string(&self.kdf).map_err(|_| VaultError::Database)?,
        )?;
        transaction.commit()?;
        self.store_dek(dek)?;
        Ok(())
    }

    fn unlock_on(&self, connection: &Connection, password: Option<&str>) -> Result<(), VaultError> {
        if self.dek.lock().map_err(|_| VaultError::Database)?.is_some() {
            return Ok(());
        }
        match stored_mode(connection)? {
            VaultMode::Keychain => self.unlock_keychain(connection),
            VaultMode::Uninitialized if self.keystore.available() => {
                self.unlock_keychain(connection)
            }
            VaultMode::Password => self.unlock_password(connection, password),
            VaultMode::Uninitialized => Err(VaultError::PasswordRequired),
        }
    }

    fn unlock_password(
        &self,
        connection: &Connection,
        password: Option<&str>,
    ) -> Result<(), VaultError> {
        if unlock_is_locked_out(connection)? {
            return Err(VaultError::LockedOut);
        }
        let password = password.ok_or(VaultError::PasswordRequired)?;
        let (wrapped, params) = read_wrapped(connection)?;
        match crypto::unwrap_dek(password, &params, &wrapped) {
            Ok(dek) if dek.len() == 32 => {
                clear_unlock_failures(connection)?;
                self.store_dek(dek)
            }
            _ => {
                if record_unlock_failure(connection)? {
                    Err(VaultError::LockedOut)
                } else {
                    Err(VaultError::InvalidPassword)
                }
            }
        }
    }

    fn unlock_keychain(&self, connection: &Connection) -> Result<(), VaultError> {
        if !self.keystore.available() {
            return Err(VaultError::Keychain);
        }
        let dek = match self.keystore.get_dek()? {
            Some(dek) if dek.len() == 32 => dek,
            Some(_) => return Err(VaultError::Keychain),
            None => {
                let dek = crypto::random_dek().map_err(|_| VaultError::Database)?;
                self.keystore.set_dek(&dek)?;
                dek
            }
        };
        if stored_mode(connection)? != VaultMode::Keychain {
            set_meta(connection, MODE_KEY, "keychain")?;
        }
        self.store_dek(dek)
    }

    fn set_credential_on(
        &self,
        connection: &Connection,
        connection_id: &str,
        secret: &CredentialSecret,
    ) -> Result<(), VaultError> {
        let dek = self.current_dek()?;
        let method = auth_method(connection, connection_id)?;
        let secret = fill_omitted_key_fields(connection, connection_id, method, secret, &dek)?;
        let payload = payload_for(method, &secret)?;
        let json = Zeroizing::new(serde_json::to_vec(&payload).map_err(|_| VaultError::Database)?);
        let (nonce, ciphertext) = crypto::encrypt(&dek, &json).map_err(|_| VaultError::Database)?;
        connection.execute(
            "INSERT INTO credential_blobs (connection_id, nonce, ciphertext, updated_at)
             VALUES (?1, ?2, ?3, datetime('now'))
             ON CONFLICT(connection_id) DO UPDATE SET
                nonce = excluded.nonce,
                ciphertext = excluded.ciphertext,
                updated_at = excluded.updated_at",
            params![connection_id, nonce, ciphertext],
        )?;
        Ok(())
    }

    fn clear_credential_on(
        &self,
        connection: &Connection,
        connection_id: &str,
    ) -> Result<(), VaultError> {
        let _ = self.current_dek()?;
        let _ = auth_method(connection, connection_id)?;
        connection.execute(
            "DELETE FROM credential_blobs WHERE connection_id = ?1",
            [connection_id],
        )?;
        Ok(())
    }

    fn has_credential_on(
        &self,
        connection: &Connection,
        connection_id: &str,
    ) -> Result<bool, VaultError> {
        let _ = auth_method(connection, connection_id)?;
        let found = connection
            .query_row(
                "SELECT 1 FROM credential_blobs WHERE connection_id = ?1",
                [connection_id],
                |_| Ok(()),
            )
            .optional()?;
        Ok(found.is_some())
    }

    fn current_dek(&self) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        self.dek
            .lock()
            .map_err(|_| VaultError::Database)?
            .clone()
            .ok_or(VaultError::Locked)
    }

    fn store_dek(&self, dek: Zeroizing<Vec<u8>>) -> Result<(), VaultError> {
        let mut slot = self.dek.lock().map_err(|_| VaultError::Database)?;
        *slot = Some(dek);
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct Payload {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    private_key_pem: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    passphrase: Option<String>,
}

impl Drop for Payload {
    fn drop(&mut self) {
        zeroize_opt(&mut self.password);
        zeroize_opt(&mut self.private_key_pem);
        zeroize_opt(&mut self.passphrase);
    }
}

fn zeroize_opt(value: &mut Option<String>) {
    if let Some(value) = value {
        value.zeroize();
    }
}

fn is_blank(value: Option<&str>) -> bool {
    value.map(str::trim).unwrap_or("").is_empty()
}

fn fill_omitted_key_fields(
    connection: &Connection,
    connection_id: &str,
    method: AuthMethod,
    secret: &CredentialSecret,
    dek: &[u8],
) -> Result<CredentialSecret, VaultError> {
    if method != AuthMethod::PrivateKey {
        return Ok(secret.clone());
    }
    if !is_blank(secret.private_key_pem.as_deref()) && !is_blank(secret.passphrase.as_deref()) {
        return Ok(secret.clone());
    }
    let Some(mut existing) = read_payload(connection, connection_id, dek)? else {
        return Ok(secret.clone());
    };
    if existing.private_key_pem.is_none() {
        return Ok(secret.clone());
    }
    let mut filled = secret.clone();
    if is_blank(filled.private_key_pem.as_deref()) {
        filled.private_key_pem = existing.private_key_pem.take();
    }
    if is_blank(filled.passphrase.as_deref()) {
        filled.passphrase = existing.passphrase.take();
    }
    Ok(filled)
}

fn read_payload(
    connection: &Connection,
    connection_id: &str,
    dek: &[u8],
) -> Result<Option<Payload>, VaultError> {
    let row: Option<(Vec<u8>, Vec<u8>)> = connection
        .query_row(
            "SELECT nonce, ciphertext FROM credential_blobs WHERE connection_id = ?1",
            [connection_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((nonce, ciphertext)) = row else {
        return Ok(None);
    };
    let plaintext = crypto::decrypt(dek, &nonce, &ciphertext).map_err(|_| VaultError::Database)?;
    let payload = serde_json::from_slice(&plaintext).map_err(|_| VaultError::Database)?;
    Ok(Some(payload))
}

fn payload_for(method: AuthMethod, secret: &CredentialSecret) -> Result<Payload, VaultError> {
    let password = clean_secret(secret.password.as_deref(), false)?;
    let private_key = clean_secret(secret.private_key_pem.as_deref(), true)?;
    let passphrase = clean_secret(secret.passphrase.as_deref(), false)?;
    match method {
        AuthMethod::Password => {
            let password =
                password.ok_or(VaultError::InvalidCredential("Password is required."))?;
            if private_key.is_some() {
                return Err(VaultError::InvalidCredential(
                    "A password connection cannot store a private key.",
                ));
            }
            if passphrase.is_some() {
                return Err(VaultError::InvalidCredential(
                    "A password connection cannot store a key passphrase.",
                ));
            }
            Ok(Payload {
                password: Some(password),
                private_key_pem: None,
                passphrase: None,
            })
        }
        AuthMethod::PrivateKey => {
            let private_key =
                private_key.ok_or(VaultError::InvalidCredential("A private key is required."))?;
            if password.is_some() {
                return Err(VaultError::InvalidCredential(
                    "A stored key cannot also keep a password.",
                ));
            }
            Ok(Payload {
                password: None,
                private_key_pem: Some(private_key),
                passphrase,
            })
        }
        AuthMethod::KeyFile => {
            if password.is_some() || private_key.is_some() {
                return Err(VaultError::InvalidCredential(
                    "A key file connection can only store a passphrase.",
                ));
            }
            let passphrase = passphrase.ok_or(VaultError::InvalidCredential(
                "A key file passphrase is required to save a secret.",
            ))?;
            Ok(Payload {
                password: None,
                private_key_pem: None,
                passphrase: Some(passphrase),
            })
        }
        AuthMethod::Agent => Err(VaultError::InvalidCredential(
            "SSH agent authentication does not store a secret.",
        )),
    }
}

fn clean_secret(value: Option<&str>, allow_newlines: bool) -> Result<Option<String>, VaultError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().any(|character| {
        character.is_control() && !(allow_newlines && (character == '\n' || character == '\r'))
    }) {
        return Err(VaultError::InvalidCredential(
            "A secret cannot contain control characters.",
        ));
    }
    if trimmed.len() > 65_536 {
        return Err(VaultError::InvalidCredential("That secret is too large."));
    }
    Ok(Some(trimmed.to_string()))
}

fn stored_mode(connection: &Connection) -> Result<VaultMode, VaultError> {
    match meta(connection, MODE_KEY)?.as_deref() {
        None => Ok(VaultMode::Uninitialized),
        Some("keychain") => Ok(VaultMode::Keychain),
        Some("password") => Ok(VaultMode::Password),
        Some(_) => Err(VaultError::Database),
    }
}

fn read_wrapped(connection: &Connection) -> Result<(WrappedDek, KdfParams), VaultError> {
    let salt = decode_meta(connection, SALT_KEY)?;
    let packed = decode_meta(connection, WRAPPED_KEY)?;
    if salt.len() < 8 || packed.len() < 13 {
        return Err(VaultError::Database);
    }
    let stored = meta(connection, PARAMS_KEY)?.ok_or(VaultError::Database)?;
    let params: KdfParams = serde_json::from_str(&stored).map_err(|_| VaultError::Database)?;
    if !accepted_kdf(&params) {
        return Err(VaultError::Database);
    }
    Ok((
        WrappedDek {
            salt,
            nonce: packed[..12].to_vec(),
            ciphertext: packed[12..].to_vec(),
        },
        params,
    ))
}

fn accepted_kdf(params: &KdfParams) -> bool {
    let floor = minimum_kdf();
    params.parallelism >= 1
        && params.parallelism <= 4
        && params.iterations >= floor.iterations
        && params.iterations <= 64
        && params.memory_kib >= floor.memory_kib.max(8 * params.parallelism)
        && params.memory_kib <= 262_144
}

fn unlock_is_locked_out(connection: &Connection) -> Result<bool, VaultError> {
    let Some(until) = meta_u64(connection, LOCKED_UNTIL_KEY)? else {
        return Ok(false);
    };
    Ok(now_secs() < until)
}

fn record_unlock_failure(connection: &Connection) -> Result<bool, VaultError> {
    let failures = meta_u64(connection, FAILURES_KEY)?.unwrap_or(0) + 1;
    set_meta(connection, FAILURES_KEY, &failures.to_string())?;
    if failures < LOCKOUT_AFTER {
        return Ok(false);
    }
    let shift = (failures - LOCKOUT_AFTER).min(9);
    let delay = 2u64
        .checked_shl(shift as u32)
        .unwrap_or(MAX_LOCKOUT_SECS)
        .min(MAX_LOCKOUT_SECS);
    set_meta(
        connection,
        LOCKED_UNTIL_KEY,
        &(now_secs().saturating_add(delay)).to_string(),
    )?;
    Ok(true)
}

fn clear_unlock_failures(connection: &Connection) -> Result<(), VaultError> {
    connection.execute(
        "DELETE FROM app_meta WHERE key = ?1 OR key = ?2",
        [FAILURES_KEY, LOCKED_UNTIL_KEY],
    )?;
    Ok(())
}

fn meta_u64(connection: &Connection, key: &str) -> Result<Option<u64>, VaultError> {
    let Some(value) = meta(connection, key)? else {
        return Ok(None);
    };
    value.parse().map(Some).map_err(|_| VaultError::Database)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn decode_meta(connection: &Connection, key: &str) -> Result<Vec<u8>, VaultError> {
    let value = meta(connection, key)?.ok_or(VaultError::Database)?;
    BASE64.decode(value).map_err(|_| VaultError::Database)
}

fn meta(connection: &Connection, key: &str) -> Result<Option<String>, VaultError> {
    let value = connection
        .query_row("SELECT value FROM app_meta WHERE key = ?1", [key], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(value)
}

fn set_meta(connection: &Connection, key: &str, value: &str) -> Result<(), VaultError> {
    connection.execute(
        "INSERT INTO app_meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn join_nonce(nonce: &[u8], ciphertext: &[u8]) -> Vec<u8> {
    let mut packed = Vec::with_capacity(nonce.len() + ciphertext.len());
    packed.extend_from_slice(nonce);
    packed.extend_from_slice(ciphertext);
    packed
}

fn auth_method(connection: &Connection, id: &str) -> Result<AuthMethod, VaultError> {
    let method: Option<String> = connection
        .query_row(
            "SELECT auth_method FROM connections WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(method) = method else {
        return Err(VaultError::NotFound);
    };
    match method.as_str() {
        "password" => Ok(AuthMethod::Password),
        "private_key" => Ok(AuthMethod::PrivateKey),
        "key_file" => Ok(AuthMethod::KeyFile),
        "agent" => Ok(AuthMethod::Agent),
        _ => Err(VaultError::Database),
    }
}

#[tauri::command]
pub fn vault_status(
    database: tauri::State<'_, Database>,
    vault: tauri::State<'_, VaultState>,
) -> Result<VaultStatus, CommandError> {
    database.with_connection(|connection| vault.status_on(connection).map_err(CommandError::from))
}

#[tauri::command]
pub fn vault_init_password(
    database: tauri::State<'_, Database>,
    vault: tauri::State<'_, VaultState>,
    password: String,
) -> Result<(), CommandError> {
    let password = Zeroizing::new(password);
    database.with_connection(|connection| {
        vault
            .init_password_on(connection, &password)
            .map_err(CommandError::from)
    })
}

#[tauri::command]
pub fn vault_unlock(
    database: tauri::State<'_, Database>,
    vault: tauri::State<'_, VaultState>,
    password: Option<String>,
) -> Result<(), CommandError> {
    let password = password.map(Zeroizing::new);
    database.with_connection(|connection| {
        vault
            .unlock_on(connection, password.as_deref().map(String::as_str))
            .map_err(CommandError::from)
    })
}

#[tauri::command]
pub fn vault_set_credential(
    database: tauri::State<'_, Database>,
    vault: tauri::State<'_, VaultState>,
    connection_id: String,
    secret: CredentialSecret,
) -> Result<(), CommandError> {
    database.with_connection(|connection| {
        vault
            .set_credential_on(connection, &connection_id, &secret)
            .map_err(CommandError::from)
    })
}

#[tauri::command]
pub fn vault_clear_credential(
    database: tauri::State<'_, Database>,
    vault: tauri::State<'_, VaultState>,
    connection_id: String,
) -> Result<(), CommandError> {
    database.with_connection(|connection| {
        vault
            .clear_credential_on(connection, &connection_id)
            .map_err(CommandError::from)
    })
}

#[tauri::command]
pub fn vault_has_credential(
    database: tauri::State<'_, Database>,
    connection_id: String,
) -> Result<bool, CommandError> {
    database.with_connection(|connection| {
        VaultState::has_credential_standalone(connection, &connection_id)
            .map_err(CommandError::from)
    })
}

impl VaultState {
    fn has_credential_standalone(
        connection: &Connection,
        connection_id: &str,
    ) -> Result<bool, VaultError> {
        let state = Self::new(Box::new(MemoryKeyStore::unavailable()), test_kdf());
        state.has_credential_on(connection, connection_id)
    }
}

#[cfg(test)]
mod tests {
    use super::{VaultMode, VaultState};
    use crate::db::apply_migrations;
    use crate::vault::crypto::{minimum_kdf, production_kdf, test_kdf};
    use crate::vault::keystore::MemoryKeyStore;
    use rusqlite::Connection;

    fn open_db() -> Connection {
        let mut connection = Connection::open_in_memory().expect("memory database");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("foreign keys");
        apply_migrations(&mut connection).expect("migrate");
        connection
    }

    fn insert_connection(connection: &Connection, id: &str, method: &str) {
        connection
            .execute(
                "INSERT INTO connections (
                    id, name, host, port, username, auth_method, created_at, updated_at
                 ) VALUES (?1, 'prod', '10.0.0.8', 22, 'ubuntu', ?2, datetime('now'), datetime('now'))",
                rusqlite::params![id, method],
            )
            .expect("insert connection");
    }

    #[test]
    fn keychain_mode_keeps_the_dek_out_of_sqlite() {
        let connection = open_db();
        let vault = VaultState::new(Box::new(MemoryKeyStore::available()), test_kdf());
        vault.unlock_on(&connection, None).expect("unlock");
        let mode: String = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key = 'vault_mode'",
                [],
                |row| row.get(0),
            )
            .expect("mode");
        let wrapped = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key = 'wrapped_dek'",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok();
        assert_eq!(mode, "keychain");
        assert!(wrapped.is_none());
        assert!(!vault.status_on(&connection).expect("status").locked);
        assert_eq!(
            vault.status_on(&connection).expect("status").mode,
            VaultMode::Keychain
        );
    }

    #[test]
    fn password_mode_rejects_the_wrong_password_and_hides_plaintext() {
        let connection = open_db();
        let vault = VaultState::new(Box::new(MemoryKeyStore::unavailable()), minimum_kdf());
        assert_eq!(
            vault.status_on(&connection).expect("status").mode,
            VaultMode::Uninitialized
        );
        vault
            .init_password_on(&connection, "correct horse")
            .expect("init");

        let locked = VaultState::new(Box::new(MemoryKeyStore::unavailable()), test_kdf());
        assert!(locked.unlock_on(&connection, Some("wrong horse")).is_err());
        locked
            .unlock_on(&connection, Some("correct horse"))
            .expect("unlock");

        insert_connection(&connection, "conn-1", "password");
        let secret = super::CredentialSecret {
            password: Some("hunter2-secret".to_string()),
            private_key_pem: None,
            passphrase: None,
        };
        locked
            .set_credential_on(&connection, "conn-1", &secret)
            .expect("set");
        assert!(
            locked
                .has_credential_on(&connection, "conn-1")
                .expect("has")
        );

        let ciphertext: Vec<u8> = connection
            .query_row(
                "SELECT ciphertext FROM credential_blobs WHERE connection_id = 'conn-1'",
                [],
                |row| row.get(0),
            )
            .expect("blob");
        let meta = connection
            .query_row(
                "SELECT COALESCE(group_concat(value, ''), '') FROM app_meta",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("meta");
        assert!(
            !ciphertext
                .windows(b"hunter2-secret".len())
                .any(|window| window == b"hunter2-secret")
        );
        assert!(!meta.contains("hunter2-secret"));
        assert!(!meta.contains("correct horse"));

        locked
            .clear_credential_on(&connection, "conn-1")
            .expect("clear");
        assert!(
            !locked
                .has_credential_on(&connection, "conn-1")
                .expect("cleared")
        );
    }

    #[test]
    fn locked_vault_refuses_to_store_a_secret() {
        let connection = open_db();
        let vault = VaultState::new(Box::new(MemoryKeyStore::unavailable()), test_kdf());
        insert_connection(&connection, "conn-1", "password");
        let secret = super::CredentialSecret {
            password: Some("hunter2-secret".to_string()),
            private_key_pem: None,
            passphrase: None,
        };
        assert!(
            vault
                .set_credential_on(&connection, "conn-1", &secret)
                .is_err()
        );
    }

    #[test]
    fn unlock_uses_the_stored_kdf_params() {
        let connection = open_db();
        let stored = minimum_kdf();
        let writer = VaultState::new(Box::new(MemoryKeyStore::unavailable()), stored);
        writer
            .init_password_on(&connection, "correct horse")
            .expect("init");

        let reader = VaultState::new(Box::new(MemoryKeyStore::unavailable()), production_kdf());
        reader
            .unlock_on(&connection, Some("correct horse"))
            .expect("stored params unlock a different process kdf");
    }

    #[test]
    fn absurd_kdf_params_are_rejected_before_derivation() {
        let connection = open_db();
        let vault = VaultState::new(Box::new(MemoryKeyStore::unavailable()), test_kdf());
        vault
            .init_password_on(&connection, "correct horse")
            .expect("init");
        connection
            .execute(
                "UPDATE app_meta SET value = ?1 WHERE key = 'kdf_params'",
                [r#"{"memory_kib":8,"iterations":1,"parallelism":1}"#],
            )
            .expect("weaken params");
        let locked = VaultState::new(Box::new(MemoryKeyStore::unavailable()), test_kdf());
        let weakened = locked
            .unlock_on(&connection, Some("correct horse"))
            .expect_err("weak params");
        assert!(matches!(weakened, super::VaultError::Database));

        connection
            .execute(
                "UPDATE app_meta SET value = ?1 WHERE key = 'kdf_params'",
                [r#"{"memory_kib":300000,"iterations":2,"parallelism":1}"#],
            )
            .expect("corrupt params");
        let oversized = locked
            .unlock_on(&connection, Some("correct horse"))
            .expect_err("oversized params");
        assert!(matches!(oversized, super::VaultError::Database));
    }

    #[test]
    fn unlock_locks_out_before_another_password_guess() {
        let connection = open_db();
        let vault = VaultState::new(Box::new(MemoryKeyStore::unavailable()), minimum_kdf());
        vault
            .init_password_on(&connection, "correct horse")
            .expect("init");
        connection
            .execute(
                "INSERT INTO app_meta (key, value) VALUES ('vault_unlock_failures', '4')",
                [],
            )
            .expect("preset failures");

        let locked = VaultState::new(Box::new(MemoryKeyStore::unavailable()), test_kdf());
        let attempt = locked
            .unlock_on(&connection, Some("wrong horse"))
            .expect_err("fifth failure");
        assert!(matches!(attempt, super::VaultError::LockedOut));
        assert_eq!(super::CommandError::from(attempt).code, "locked_out");

        connection
            .execute(
                "UPDATE app_meta SET value = '8' WHERE key = 'kdf_params'",
                [],
            )
            .expect("weaken after lockout");
        let blocked = locked
            .unlock_on(&connection, Some("correct horse"))
            .expect_err("locked out");
        assert!(matches!(blocked, super::VaultError::LockedOut));
    }

    #[test]
    fn password_init_is_refused_when_a_keystore_is_available() {
        let connection = open_db();
        let vault = VaultState::new(Box::new(MemoryKeyStore::available()), test_kdf());
        assert!(
            vault
                .init_password_on(&connection, "correct horse")
                .is_err()
        );
        assert_eq!(
            vault.status_on(&connection).expect("status").mode,
            VaultMode::Uninitialized
        );
    }

    #[test]
    fn a_second_password_init_does_not_look_like_a_bad_password() {
        let connection = open_db();
        let vault = VaultState::new(Box::new(MemoryKeyStore::unavailable()), test_kdf());
        vault
            .init_password_on(&connection, "correct horse")
            .expect("init");
        let error = vault
            .init_password_on(&connection, "another horse")
            .expect_err("second init");
        let command = super::CommandError::from(error);
        assert_eq!(command.code, "already_initialized");
    }

    #[test]
    fn keychain_reuses_the_existing_dek() {
        let connection = open_db();
        let store = std::sync::Arc::new(MemoryKeyStore::available());
        let first = VaultState::new(Box::new(SharedStore(store.clone())), test_kdf());
        first.unlock_on(&connection, None).expect("first unlock");
        insert_connection(&connection, "conn-1", "password");
        let secret = super::CredentialSecret {
            password: Some("hunter2-secret".to_string()),
            private_key_pem: None,
            passphrase: None,
        };
        first
            .set_credential_on(&connection, "conn-1", &secret)
            .expect("set");

        let second = VaultState::new(Box::new(SharedStore(store)), test_kdf());
        second.unlock_on(&connection, None).expect("second unlock");
        let dek = second.current_dek().expect("dek");
        let payload = super::read_payload(&connection, "conn-1", &dek)
            .expect("decrypt")
            .expect("payload");
        assert_eq!(payload.password.as_deref(), Some("hunter2-secret"));
    }

    #[test]
    fn omitted_private_key_fields_keep_the_saved_secret() {
        let connection = open_db();
        let vault = VaultState::new(Box::new(MemoryKeyStore::unavailable()), test_kdf());
        vault
            .init_password_on(&connection, "correct horse")
            .expect("init");
        insert_connection(&connection, "conn-1", "private_key");
        vault
            .set_credential_on(
                &connection,
                "conn-1",
                &super::CredentialSecret {
                    password: None,
                    private_key_pem: Some("-----BEGIN OPENSSH PRIVATE KEY-----\nkey\n".to_string()),
                    passphrase: Some("fixture-a".to_string()),
                },
            )
            .expect("set both");
        vault
            .set_credential_on(
                &connection,
                "conn-1",
                &super::CredentialSecret {
                    password: None,
                    private_key_pem: None,
                    passphrase: Some("fixture-b".to_string()),
                },
            )
            .expect("replace passphrase only");

        let dek = vault.current_dek().expect("dek");
        let payload = super::read_payload(&connection, "conn-1", &dek)
            .expect("decrypt")
            .expect("payload");
        assert_eq!(
            payload.private_key_pem.as_deref(),
            Some("-----BEGIN OPENSSH PRIVATE KEY-----\nkey")
        );
        assert_eq!(payload.passphrase.as_deref(), Some("fixture-b"));
        assert!(payload.password.is_none());
    }

    #[test]
    fn secrets_must_match_the_auth_method() {
        let connection = open_db();
        let vault = VaultState::new(Box::new(MemoryKeyStore::unavailable()), test_kdf());
        vault
            .init_password_on(&connection, "correct horse")
            .expect("init");
        insert_connection(&connection, "conn-1", "agent");
        let rejected = vault.set_credential_on(
            &connection,
            "conn-1",
            &super::CredentialSecret {
                password: Some("hunter2-secret".to_string()),
                private_key_pem: None,
                passphrase: None,
            },
        );
        assert!(rejected.is_err());
        assert!(!vault.has_credential_on(&connection, "conn-1").expect("has"));
    }

    #[test]
    fn deleting_a_connection_removes_its_credential_blob() {
        let connection = open_db();
        let vault = VaultState::new(Box::new(MemoryKeyStore::unavailable()), test_kdf());
        vault
            .init_password_on(&connection, "correct horse")
            .expect("init");
        insert_connection(&connection, "conn-1", "password");
        vault
            .set_credential_on(
                &connection,
                "conn-1",
                &super::CredentialSecret {
                    password: Some("hunter2-secret".to_string()),
                    private_key_pem: None,
                    passphrase: None,
                },
            )
            .expect("set");
        connection
            .execute("DELETE FROM connections WHERE id = 'conn-1'", [])
            .expect("delete");
        let remaining: i64 = connection
            .query_row("SELECT COUNT(*) FROM credential_blobs", [], |row| {
                row.get(0)
            })
            .expect("count");
        assert_eq!(remaining, 0);
    }

    struct SharedStore(std::sync::Arc<MemoryKeyStore>);

    impl crate::vault::keystore::KeyStore for SharedStore {
        fn available(&self) -> bool {
            self.0.available()
        }

        fn get_dek(
            &self,
        ) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, crate::vault::keystore::KeyStoreError>
        {
            self.0.get_dek()
        }

        fn set_dek(&self, dek: &[u8]) -> Result<(), crate::vault::keystore::KeyStoreError> {
            self.0.set_dek(dek)
        }
    }
}
