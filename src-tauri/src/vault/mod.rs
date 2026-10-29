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
use crate::vault::crypto::{KdfParams, WrappedDek, production_kdf, test_kdf};
use crate::vault::keystore::{KeyStore, KeyStoreError, MemoryKeyStore, OsKeyStore};

const MODE_KEY: &str = "vault_mode";
const WRAPPED_KEY: &str = "wrapped_dek";
const SALT_KEY: &str = "kdf_salt";
const PARAMS_KEY: &str = "kdf_params";

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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSecret {
    pub password: Option<String>,
    pub private_key_pem: Option<String>,
    pub passphrase: Option<String>,
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
        if stored_mode(connection)? != VaultMode::Uninitialized {
            return Err(VaultError::InvalidPassword);
        }
        if password.chars().count() < 8 {
            return Err(VaultError::InvalidCredential(
                "Master password must be at least 8 characters.",
            ));
        }
        let dek = crypto::random_dek().map_err(|_| VaultError::Database)?;
        let wrapped =
            crypto::wrap_dek(password, &self.kdf, &dek).map_err(|_| VaultError::Database)?;
        set_meta(connection, MODE_KEY, "password")?;
        set_meta(connection, SALT_KEY, &BASE64.encode(&wrapped.salt))?;
        set_meta(
            connection,
            WRAPPED_KEY,
            &BASE64.encode(join_nonce(&wrapped.nonce, &wrapped.ciphertext)),
        )?;
        set_meta(
            connection,
            PARAMS_KEY,
            &serde_json::to_string(&self.kdf).map_err(|_| VaultError::Database)?,
        )?;
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
            VaultMode::Password => {
                let password = password.ok_or(VaultError::PasswordRequired)?;
                let wrapped = read_wrapped(connection)?;
                let dek = crypto::unwrap_dek(password, &self.kdf, &wrapped)
                    .map_err(|_| VaultError::InvalidPassword)?;
                self.store_dek(dek)
            }
            VaultMode::Uninitialized => Err(VaultError::PasswordRequired),
        }
    }

    fn unlock_keychain(&self, connection: &Connection) -> Result<(), VaultError> {
        if !self.keystore.available() {
            return Err(VaultError::Keychain);
        }
        if stored_mode(connection)? != VaultMode::Keychain {
            set_meta(connection, MODE_KEY, "keychain")?;
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
        let payload = payload_for(method, secret)?;
        let json = serde_json::to_vec(&payload).map_err(|_| VaultError::Database)?;
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

#[derive(Serialize)]
struct Payload {
    #[serde(skip_serializing_if = "Option::is_none")]
    password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    private_key_pem: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    passphrase: Option<String>,
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
            Ok(Payload {
                password: Some(password),
                private_key_pem: None,
                passphrase,
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

fn read_wrapped(connection: &Connection) -> Result<WrappedDek, VaultError> {
    let salt = decode_meta(connection, SALT_KEY)?;
    let packed = decode_meta(connection, WRAPPED_KEY)?;
    if packed.len() < 13 {
        return Err(VaultError::Database);
    }
    let stored: KdfParams =
        serde_json::from_str(&meta(connection, PARAMS_KEY)?.unwrap_or_default())
            .map_err(|_| VaultError::Database)?;
    let _ = stored;
    Ok(WrappedDek {
        salt,
        nonce: packed[..12].to_vec(),
        ciphertext: packed[12..].to_vec(),
    })
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
    let mut password = password;
    let result = database.with_connection(|connection| {
        vault
            .init_password_on(connection, &password)
            .map_err(CommandError::from)
    });
    password.zeroize();
    result
}

#[tauri::command]
pub fn vault_unlock(
    database: tauri::State<'_, Database>,
    vault: tauri::State<'_, VaultState>,
    password: Option<String>,
) -> Result<(), CommandError> {
    let mut password = password;
    let result = database.with_connection(|connection| {
        vault
            .unlock_on(connection, password.as_deref())
            .map_err(CommandError::from)
    });
    if let Some(value) = &mut password {
        value.zeroize();
    }
    result
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
    use crate::vault::crypto::test_kdf;
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
        let vault = VaultState::new(Box::new(MemoryKeyStore::unavailable()), test_kdf());
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
}
