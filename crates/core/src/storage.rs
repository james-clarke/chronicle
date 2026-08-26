use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use rusqlite::Connection;
use rusqlite_migration::{M, Migrations};

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("failed to create data dir: {0}")]
    Io(#[from] std::io::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("migration error: {0}")]
    Migration(#[from] rusqlite_migration::Error),
}

static MIGRATIONS: LazyLock<Migrations<'static>> =
    LazyLock::new(|| Migrations::new(vec![M::up(include_str!("../migrations/001_schema.sql"))]));

pub fn open(path: &Path) -> Result<Connection, StorageError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut conn = Connection::open(path)?;
    // auto_vacuum must be set before the first table exists; it's a no-op on
    // an already-populated db (flipping it later requires a full VACUUM).
    conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
    conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    MIGRATIONS.to_latest(&mut conn)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    #[test]
    fn migrations_are_valid() {
        assert!(super::MIGRATIONS.validate().is_ok());
    }
}
