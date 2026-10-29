use rusqlite::{Connection as Sqlite, OptionalExtension, params};

use super::{AuthMethod, Connection, ValidatedConnection};

#[derive(Debug)]
pub enum RepoError {
    NotFound,
    Database(#[allow(dead_code)] rusqlite::Error),
}

impl From<rusqlite::Error> for RepoError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

pub fn list(connection: &Sqlite) -> Result<Vec<Connection>, RepoError> {
    let mut statement = connection.prepare(
        "SELECT id, name, host, port, username, auth_method, key_path, default_path, notes,
                created_at, updated_at, last_used_at
         FROM connections
         ORDER BY name COLLATE NOCASE, host COLLATE NOCASE, id",
    )?;
    let rows = statement.query_map([], map_row)?;
    let mut connections = Vec::new();
    for row in rows {
        connections.push(row?);
    }
    Ok(connections)
}

pub fn save(connection: &Sqlite, record: ValidatedConnection) -> Result<Connection, RepoError> {
    let Some(id) = record.id else {
        let id = uuid::Uuid::new_v4().to_string();
        connection.execute(
            "INSERT INTO connections (
                id, name, host, port, username, auth_method, key_path, default_path, notes,
                created_at, updated_at, last_used_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, datetime('now'), datetime('now'), NULL)",
            params![
                id,
                record.name,
                record.host,
                record.port,
                record.username,
                record.auth_method.as_str(),
                record.key_path,
                record.default_path,
                record.notes,
            ],
        )?;
        return get(connection, &id)?.ok_or(RepoError::NotFound);
    };

    let changed = connection.execute(
        "UPDATE connections
         SET name = ?1, host = ?2, port = ?3, username = ?4, auth_method = ?5,
             key_path = ?6, default_path = ?7, notes = ?8, updated_at = datetime('now')
         WHERE id = ?9",
        params![
            record.name,
            record.host,
            record.port,
            record.username,
            record.auth_method.as_str(),
            record.key_path,
            record.default_path,
            record.notes,
            id,
        ],
    )?;
    if changed == 0 {
        return Err(RepoError::NotFound);
    }
    get(connection, &id)?.ok_or(RepoError::NotFound)
}

pub fn delete(connection: &Sqlite, id: &str) -> Result<(), RepoError> {
    let changed = connection.execute("DELETE FROM connections WHERE id = ?1", [id])?;
    if changed == 0 {
        Err(RepoError::NotFound)
    } else {
        Ok(())
    }
}

fn get(connection: &Sqlite, id: &str) -> Result<Option<Connection>, RepoError> {
    let mut statement = connection.prepare(
        "SELECT id, name, host, port, username, auth_method, key_path, default_path, notes,
                created_at, updated_at, last_used_at
         FROM connections
         WHERE id = ?1",
    )?;
    let row = statement.query_row([id], map_row).optional()?;
    Ok(row)
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Connection> {
    let auth_method: String = row.get(5)?;
    let auth_method = AuthMethod::parse(&auth_method).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            5,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other("unknown auth method")),
        )
    })?;
    Ok(Connection {
        id: row.get(0)?,
        name: row.get(1)?,
        host: row.get(2)?,
        port: row.get(3)?,
        username: row.get(4)?,
        auth_method,
        key_path: row.get(6)?,
        default_path: row.get(7)?,
        notes: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
        last_used_at: row.get(11)?,
    })
}
