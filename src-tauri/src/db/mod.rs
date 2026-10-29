mod migrate;

use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;
use tauri::{AppHandle, Manager};

use crate::db::migrate::MigrateError;

#[derive(Debug)]
pub struct DbError {
    message: String,
}

impl std::fmt::Display for DbError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for DbError {}

impl From<MigrateError> for DbError {
    fn from(error: MigrateError) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(error: rusqlite::Error) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

impl From<std::io::Error> for DbError {
    fn from(error: std::io::Error) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

pub struct Database {
    connection: Mutex<Connection>,
}

pub fn latest_schema_version() -> String {
    migrate::latest_version().to_string()
}

impl Database {
    pub fn schema_version(&self) -> Result<String, DbError> {
        let connection = self.connection.lock().map_err(|_| DbError {
            message: "database lock poisoned".to_string(),
        })?;
        let version = connection.query_row(
            "SELECT value FROM app_meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )?;
        Ok(version)
    }
}

pub fn open(app: &AppHandle) -> Result<Database, DbError> {
    let directory = app.path().app_data_dir().map_err(|error| DbError {
        message: error.to_string(),
    })?;
    open_at(&directory.join("filebridge.db"))
}

pub fn open_at(path: &Path) -> Result<Database, DbError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
        // Journals sit beside the database. A private directory covers those files too.
        restrict_directory(parent)?;
    }

    let mut connection = Connection::open(path)?;
    restrict_permissions(path)?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "busy_timeout", 5_000)?;
    migrate::migrate(&mut connection)?;
    Ok(Database {
        connection: Mutex::new(connection),
    })
}

#[cfg(unix)]
fn restrict_directory(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::open_at;
    use rusqlite::Connection;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempDb {
        directory: PathBuf,
        path: PathBuf,
    }

    impl TempDb {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let directory = std::env::temp_dir().join(format!("filebridge-db-{unique}"));
            let path = directory.join("filebridge.db");
            Self { directory, path }
        }
    }

    impl Drop for TempDb {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn data_directory_override_is_filebridge() {
        let config = include_str!("../../tauri.conf.json");
        assert!(
            config.contains("\"$DATA/filebridge\""),
            "database must live in the filebridge app data directory"
        );
    }

    #[test]
    fn open_at_twice_does_not_duplicate() {
        let temp = TempDb::new();

        open_at(&temp.path).expect("first open");
        open_at(&temp.path).expect("second open");

        let connection = Connection::open(&temp.path).expect("reopen");
        let migration_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("count migrations");
        drop(connection);

        assert_eq!(migration_count, 1);
    }

    #[cfg(unix)]
    #[test]
    fn database_directory_and_file_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let temp = TempDb::new();
        let database = open_at(&temp.path).expect("open");
        {
            let connection = database.connection.lock().expect("lock");
            connection
                .execute_batch(
                    "BEGIN IMMEDIATE;
                     CREATE TABLE scratch (id INTEGER);
                     INSERT INTO scratch VALUES (1);",
                )
                .expect("write");
            let journal = temp.path.with_extension("db-journal");
            if journal.exists() {
                let journal_mode = std::fs::metadata(&journal)
                    .expect("journal metadata")
                    .permissions()
                    .mode()
                    & 0o777;
                assert_eq!(
                    journal_mode & 0o077,
                    0,
                    "journal mode {journal_mode:o} is readable by other users"
                );
            }
            connection.execute_batch("ROLLBACK;").expect("rollback");
        }

        let directory_mode = std::fs::metadata(&temp.directory)
            .expect("directory metadata")
            .permissions()
            .mode()
            & 0o777;
        let file_mode = std::fs::metadata(&temp.path)
            .expect("file metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(directory_mode, 0o700);
        assert_eq!(file_mode, 0o600);
    }
}
