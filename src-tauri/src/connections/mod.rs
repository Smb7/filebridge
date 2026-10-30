mod repo;
mod validate;

use serde::{Deserialize, Serialize};

use crate::db::{Database, DbError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod {
    Password,
    PrivateKey,
    KeyFile,
    Agent,
}

impl AuthMethod {
    fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::PrivateKey => "private_key",
            Self::KeyFile => "key_file",
            Self::Agent => "agent",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "password" => Some(Self::Password),
            "private_key" => Some(Self::PrivateKey),
            "key_file" => Some(Self::KeyFile),
            "agent" => Some(Self::Agent),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Connection {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: AuthMethod,
    pub key_path: Option<String>,
    pub default_path: Option<String>,
    pub notes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionDraft {
    pub id: Option<String>,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub auth_method: AuthMethod,
    pub key_path: Option<String>,
    pub default_path: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug)]
struct ValidatedConnection {
    id: Option<String>,
    name: String,
    host: String,
    port: u16,
    username: String,
    auth_method: AuthMethod,
    key_path: Option<String>,
    default_path: Option<String>,
    notes: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CommandError {
    pub code: &'static str,
    pub message: String,
}

impl From<validate::ValidationError> for CommandError {
    fn from(error: validate::ValidationError) -> Self {
        Self {
            code: "invalid_connection",
            message: error.message,
        }
    }
}

impl From<repo::RepoError> for CommandError {
    fn from(error: repo::RepoError) -> Self {
        match error {
            repo::RepoError::NotFound => Self {
                code: "not_found",
                message: "Connection not found.".to_string(),
            },
            repo::RepoError::Database(_) => Self {
                code: "database",
                message: "Could not update connections.".to_string(),
            },
        }
    }
}

impl From<DbError> for CommandError {
    fn from(error: DbError) -> Self {
        let _ = error;
        Self {
            code: "database",
            message: "Could not update connections.".to_string(),
        }
    }
}

pub(crate) fn saved(
    connection: &rusqlite::Connection,
    id: &str,
) -> Result<Option<Connection>, repo::RepoError> {
    repo::get(connection, id)
}

pub(crate) fn touch_used(
    connection: &rusqlite::Connection,
    id: &str,
) -> Result<(), repo::RepoError> {
    repo::touch_used(connection, id)
}

pub(crate) enum RemoveError {
    NotFound,
    Database,
}

pub(crate) fn remove(connection: &rusqlite::Connection, id: &str) -> Result<(), RemoveError> {
    repo::delete(connection, id).map_err(|error| match error {
        repo::RepoError::NotFound => RemoveError::NotFound,
        repo::RepoError::Database(_) => RemoveError::Database,
    })
}

#[tauri::command]
pub fn connections_list(
    database: tauri::State<'_, Database>,
) -> Result<Vec<Connection>, CommandError> {
    database.with_connection(|connection| repo::list(connection).map_err(CommandError::from))
}

#[tauri::command]
pub fn connections_save(
    database: tauri::State<'_, Database>,
    draft: ConnectionDraft,
) -> Result<Connection, CommandError> {
    let validated = validate::validate(draft)?;
    database
        .with_connection(|connection| repo::save(connection, validated).map_err(CommandError::from))
}

#[tauri::command]
pub async fn connections_delete(
    database: tauri::State<'_, Database>,
    sessions: tauri::State<'_, crate::session::SessionState>,
    id: String,
) -> Result<(), CommandError> {
    // Close any live SSH session under the same lock that connect uses, then
    // delete the row. Otherwise a connect in flight can revive a deleted server.
    sessions.delete_connection(&database, &id).await
}

#[cfg(test)]
mod tests {
    use super::{AuthMethod, ConnectionDraft, repo, validate};
    use crate::db::apply_migrations;
    use rusqlite::Connection as Sqlite;

    fn draft() -> ConnectionDraft {
        ConnectionDraft {
            id: None,
            name: "prod-telegraf".to_string(),
            host: "10.0.0.8".to_string(),
            port: 22,
            username: "ubuntu".to_string(),
            auth_method: AuthMethod::Agent,
            key_path: None,
            default_path: Some("/etc/telegraf".to_string()),
            notes: None,
        }
    }

    fn open_db() -> Sqlite {
        let mut connection = Sqlite::open_in_memory().expect("memory database");
        apply_migrations(&mut connection).expect("migrate");
        connection
    }

    #[test]
    fn saves_lists_updates_and_deletes_without_storing_a_secret() {
        let connection = open_db();
        let created =
            repo::save(&connection, validate::validate(draft()).expect("valid")).expect("insert");
        assert!(!created.id.is_empty());
        assert_eq!(created.auth_method, AuthMethod::Agent);
        assert_eq!(created.key_path, None);
        assert!(created.last_used_at.is_none());

        let mut update = draft();
        update.id = Some(created.id.clone());
        update.name = "'); DROP TABLE connections; --".to_string();
        update.host = "10.0.0.9".to_string();
        let updated = repo::save(
            &connection,
            validate::validate(update).expect("valid update"),
        )
        .expect("update");
        assert_eq!(updated.id, created.id);
        assert_eq!(updated.name, "'); DROP TABLE connections; --");
        assert_eq!(updated.created_at, created.created_at);

        let listed = repo::list(&connection).expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].host, "10.0.0.9");
        let table_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'connections'",
                [],
                |row| row.get(0),
            )
            .expect("table still exists");
        assert_eq!(table_count, 1);

        repo::delete(&connection, &created.id).expect("delete");
        assert!(repo::list(&connection).expect("empty").is_empty());
        let missing = repo::delete(&connection, &created.id).expect_err("missing");
        assert!(matches!(missing, repo::RepoError::NotFound));
    }

    #[test]
    fn update_of_unknown_id_is_not_found() {
        let connection = open_db();
        let mut update = draft();
        update.id = Some("missing".to_string());
        let error = repo::save(&connection, validate::validate(update).expect("valid"))
            .expect_err("missing id");
        assert!(matches!(error, repo::RepoError::NotFound));
    }
}
