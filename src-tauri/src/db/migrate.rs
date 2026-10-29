use rusqlite::{Connection, OptionalExtension};

const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("../../migrations/001_init.sql")),
    (2, include_str!("../../migrations/002_connections.sql")),
];

#[derive(Debug)]
pub struct MigrateError {
    message: String,
}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MigrateError {}

impl From<rusqlite::Error> for MigrateError {
    fn from(error: rusqlite::Error) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

pub fn latest_version() -> i64 {
    MIGRATIONS.last().map(|(version, _)| *version).unwrap_or(0)
}

pub fn migrate(connection: &mut Connection) -> Result<(), MigrateError> {
    apply(connection, MIGRATIONS)
}

fn apply(connection: &mut Connection, migrations: &[(i64, &str)]) -> Result<(), MigrateError> {
    ensure_increasing(migrations)?;
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            applied_at TEXT NOT NULL
        );",
    )?;

    let applied = applied_versions(connection)?;
    if !is_prefix(&applied, migrations) {
        return Err(MigrateError {
            message: "database schema is newer than this build".to_string(),
        });
    }

    for (version, sql) in migrations.iter().skip(applied.len()) {
        let transaction = connection.transaction()?;
        transaction.execute_batch(sql)?;
        transaction.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, datetime('now'))",
            [version],
        )?;
        let version_text = version.to_string();
        transaction.execute(
            "INSERT INTO app_meta (key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [&version_text],
        )?;
        transaction.commit()?;
    }

    if let Some((latest, _)) = migrations.last() {
        let stored: Option<String> = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if stored.as_deref() != Some(latest.to_string().as_str()) {
            return Err(MigrateError {
                message: "schema version does not match applied migrations".to_string(),
            });
        }
    }

    Ok(())
}

fn ensure_increasing(migrations: &[(i64, &str)]) -> Result<(), MigrateError> {
    let mut previous = 0;
    for (version, _) in migrations {
        if *version <= previous {
            return Err(MigrateError {
                message: format!("migration versions must increase, found {version}"),
            });
        }
        previous = *version;
    }
    Ok(())
}

fn applied_versions(connection: &Connection) -> Result<Vec<i64>, MigrateError> {
    let mut statement =
        connection.prepare("SELECT version FROM schema_migrations ORDER BY version")?;
    let rows = statement.query_map([], |row| row.get(0))?;
    let mut versions = Vec::new();
    for version in rows {
        versions.push(version?);
    }
    Ok(versions)
}

fn is_prefix(applied: &[i64], migrations: &[(i64, &str)]) -> bool {
    if applied.len() > migrations.len() {
        return false;
    }
    applied
        .iter()
        .zip(migrations)
        .all(|(applied_version, (known, _))| applied_version == known)
}

#[cfg(test)]
mod tests {
    use super::{apply, latest_version, migrate};
    use rusqlite::Connection;

    const CREATE_META: &str = "CREATE TABLE app_meta (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );";

    #[test]
    fn migrate_twice_does_not_duplicate() {
        let mut connection = Connection::open_in_memory().expect("open in-memory database");

        migrate(&mut connection).expect("first migrate");
        migrate(&mut connection).expect("second migrate");

        let migration_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("count migrations");
        let meta_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM app_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .expect("count schema version");
        let schema_version: String = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .expect("read schema version");

        assert_eq!(migration_count, latest_version());
        assert_eq!(meta_count, 1);
        assert_eq!(schema_version, latest_version().to_string());
    }

    #[test]
    fn failed_migration_is_not_recorded() {
        let mut connection = Connection::open_in_memory().expect("open in-memory database");

        let error = apply(&mut connection, &[(1, "NOT VALID SQL;")]).expect_err("bad sql");

        let migration_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("count migrations");
        assert!(!error.to_string().is_empty());
        assert_eq!(migration_count, 0);
    }

    #[test]
    fn refuses_a_database_newer_than_this_build() {
        let mut connection = Connection::open_in_memory().expect("open in-memory database");
        apply(
            &mut connection,
            &[
                (1, CREATE_META),
                (
                    2,
                    "INSERT INTO app_meta (key, value) VALUES ('marker', '2');",
                ),
            ],
        )
        .expect("apply a newer schema");

        let error = apply(&mut connection, &[(1, CREATE_META)]).expect_err("downgrade");
        let marker: String = connection
            .query_row(
                "SELECT value FROM app_meta WHERE key = 'marker'",
                [],
                |row| row.get(0),
            )
            .expect("marker survives the refusal");

        assert!(error.to_string().contains("newer"));
        assert_eq!(marker, "2");
    }

    #[test]
    fn rejects_versions_that_do_not_increase() {
        let mut connection = Connection::open_in_memory().expect("open in-memory database");

        let error = apply(&mut connection, &[(2, CREATE_META), (2, CREATE_META)])
            .expect_err("duplicate version");

        assert!(error.to_string().contains("increase"));
    }
}
