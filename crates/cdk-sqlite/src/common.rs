use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cdk_sql_common::pool::{self, DatabasePool};
use cdk_sql_common::value::Value;
use rusqlite::{ffi, Connection};

use crate::async_sqlite;

/// Error returned when a [`SqliteConfig`] cannot be built
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SqliteConfigError {
    /// The database file path is empty
    #[error("SQLite database path is empty")]
    EmptyPath,
}

/// A non-empty path to the database file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DatabaseFile(PathBuf);

impl DatabaseFile {
    fn new(path: PathBuf) -> Result<Self, SqliteConfigError> {
        if path.as_os_str().is_empty() {
            return Err(SqliteConfigError::EmptyPath);
        }

        Ok(Self(path))
    }

    /// The name handed to SQLite.
    fn sqlite_name(&self) -> &Path {
        &self.0
    }
}

/// An in-memory database lives inside a single connection.
const MEMORY_CONNECTIONS_MAX: usize = 1;

// The discarded-connection guard reads a second open as a lost database. That
// holds only while the pool keeps a single connection.
const _: () = assert!(MEMORY_CONNECTIONS_MAX == 1);

/// Where the database lives.
enum Location {
    File(DatabaseFile),
    /// The database exists only inside its single connection.
    Memory {
        /// Set once opened, so a second open means that connection was discarded.
        opened: AtomicBool,
    },
}

impl Clone for Location {
    fn clone(&self) -> Self {
        match self {
            Self::File(file) => Self::File(file.clone()),
            // A cloned config describes a new pool with its own database.
            Self::Memory { .. } => Self::Memory {
                opened: AtomicBool::new(false),
            },
        }
    }
}

impl fmt::Debug for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::File(file) => f.debug_tuple("File").field(&file.sqlite_name()).finish(),
            Self::Memory { .. } => f.write_str("Memory"),
        }
    }
}

/// The config needed to open SQLite connections.
///
/// Build it with [`SqliteConfig::file`] or [`SqliteConfig::memory`]. Every
/// connection opened from one config reaches the same database.
#[derive(Clone)]
pub struct SqliteConfig {
    location: Location,
    password: Option<String>,
}

impl SqliteConfig {
    /// A database stored in the file at `path`.
    pub fn file<P>(path: P) -> Result<Self, SqliteConfigError>
    where
        P: Into<PathBuf>,
    {
        Ok(Self {
            location: Location::File(DatabaseFile::new(path.into())?),
            password: None,
        })
    }

    /// A database held in memory and lost when it is dropped.
    ///
    /// It has a single connection. If that connection is discarded, every later
    /// operation fails instead of silently starting over with an empty database.
    pub fn memory() -> Self {
        Self {
            location: Location::Memory {
                opened: AtomicBool::new(false),
            },
            password: None,
        }
    }

    /// Set the SQLCipher key used to open the database.
    pub fn with_password<P>(mut self, password: P) -> Self
    where
        P: Into<String>,
    {
        self.password = Some(password.into());
        self
    }
}

impl fmt::Debug for SqliteConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqliteConfig")
            .field("location", &self.location)
            .field("password", &self.password.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

impl pool::DatabaseConfig for SqliteConfig {
    fn default_timeout(&self) -> Duration {
        Duration::from_secs(5)
    }

    fn max_size(&self) -> usize {
        match self.location {
            Location::File(_) => 20,
            Location::Memory { .. } => MEMORY_CONNECTIONS_MAX,
        }
    }
}

fn cant_open(message: &str) -> pool::Error<rusqlite::Error> {
    pool::Error::Resource(rusqlite::Error::SqliteFailure(
        ffi::Error::new(ffi::SQLITE_CANTOPEN),
        Some(message.to_owned()),
    ))
}

fn open_file(file: &DatabaseFile) -> Result<Connection, pool::Error<rusqlite::Error>> {
    let name = file.sqlite_name();
    if let Some(parent) = name.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return Err(pool::Error::Resource(rusqlite::Error::InvalidPath(
                name.to_path_buf(),
            )));
        }
    }

    Ok(Connection::open(name)?)
}

/// Sqlite connection manager
#[derive(Debug)]
pub struct SqliteConnectionManager;

impl DatabasePool for SqliteConnectionManager {
    type Config = SqliteConfig;

    type Connection = async_sqlite::AsyncSqlite;

    type Error = rusqlite::Error;

    fn drop(resource: Self::Connection) {
        resource.close();
    }

    fn new_resource(
        config: &Self::Config,
        stale: Arc<AtomicBool>,
        _timeout: Duration,
    ) -> Result<Self::Connection, pool::Error<Self::Error>> {
        let conn = match &config.location {
            Location::File(file) => open_file(file)?,
            Location::Memory { opened } => {
                if opened.load(Ordering::SeqCst) {
                    // Handing out a fresh, empty database would silently drop
                    // every previous write, so make all later operations fail.
                    tracing::error!(
                        "In-memory SQLite connection was discarded; its database is lost"
                    );
                    return Err(cant_open(
                        "in-memory database was lost after its connection was discarded",
                    ));
                }
                Connection::open_in_memory()?
            }
        };

        if let Some(password) = config.password.as_ref() {
            conn.pragma_update(None, "key", password)?;
        }

        conn.execute_batch(
            r#"
            pragma busy_timeout = 10000;
            pragma journal_mode = WAL;
            pragma synchronous = FULL;
            pragma temp_store = memory;
            pragma mmap_size = 5242880;
            pragma cache = shared;
            "#,
        )?;

        conn.busy_timeout(Duration::from_secs(10))?;

        if let Location::Memory { opened } = &config.location {
            opened.store(true, Ordering::SeqCst);
        }

        Ok(async_sqlite::AsyncSqlite::new(conn, stale))
    }
}

/// Convert cdk_sql_common::value::Value to rusqlite Value
#[inline(always)]
pub fn to_sqlite(v: Value) -> rusqlite::types::Value {
    match v {
        Value::Blob(blob) => rusqlite::types::Value::Blob(blob),
        Value::Integer(i) => rusqlite::types::Value::Integer(i),
        Value::Null => rusqlite::types::Value::Null,
        Value::Text(t) => rusqlite::types::Value::Text(t),
        Value::Real(r) => rusqlite::types::Value::Real(r),
    }
}

/// Convert from rusqlite Valute to cdk_sql_common::value::Value
#[inline(always)]
pub fn from_sqlite(v: rusqlite::types::Value) -> Value {
    match v {
        rusqlite::types::Value::Blob(blob) => Value::Blob(blob),
        rusqlite::types::Value::Integer(i) => Value::Integer(i),
        rusqlite::types::Value::Null => Value::Null,
        rusqlite::types::Value::Text(t) => Value::Text(t),
        rusqlite::types::Value::Real(r) => Value::Real(r),
    }
}

#[cfg(test)]
mod tests {
    use super::{SqliteConfig, SqliteConfigError};

    #[test]
    fn config_debug_redacts_sqlcipher_password() {
        let secret = "sqlcipher-password-secret";
        let config = SqliteConfig::file("wallet.sqlite")
            .expect("valid path")
            .with_password(secret);

        let debug = format!("{config:?}");

        assert!(debug.contains("wallet.sqlite"));
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(secret));
    }

    #[test]
    fn empty_path_is_rejected() {
        assert_eq!(
            SqliteConfig::file("").map(|_| ()),
            Err(SqliteConfigError::EmptyPath)
        );
    }
}
