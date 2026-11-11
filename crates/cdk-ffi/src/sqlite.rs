use std::sync::Arc;

use cdk_common::database::{Error as CdkDatabaseError, WalletDatabase as CdkWalletDatabase};
use cdk_sqlite::wallet::WalletSqliteDatabase as CdkWalletSqliteDatabase;
use cdk_sqlite::SqliteConfig;

use crate::{
    CurrencyUnit, FfiError, FfiWalletDatabaseWrapper, Id, KeySet, KeySetInfo, Keys, MeltQuote,
    MintInfo, MintQuote, MintUrl, P2PKSigningKey, ProofInfo, ProofState, PublicKey,
    SpendingConditions, Transaction, TransactionDirection, TransactionId, WalletDatabase,
};

/// FFI-compatible SQLite wallet database.
///
/// Wallet methods can write to this database from FFI calls that mint, receive,
/// recover, subscribe, or check quote/proof state. Mobile host apps own
/// lifecycle handling for the database file: choose a durable app-owned path,
/// avoid interrupting writes during background transitions, and use platform
/// facilities such as iOS `beginBackgroundTask` when an operation must finish
/// after backgrounding.
#[derive(uniffi::Object)]
pub struct WalletSqliteDatabase {
    inner: Arc<FfiWalletDatabaseWrapper<CdkWalletSqliteDatabase, CdkDatabaseError>>,
    // Keep the runtime alive so async pool operations work in FFI contexts.
    _runtime: crate::runtime::RuntimeGuard,
}

#[uniffi::export]
impl WalletSqliteDatabase {
    /// Create a new SQLite wallet database at `file_path`.
    ///
    /// Wallet operations may later write to this database. Mobile hosts are
    /// responsible for choosing a durable file location and coordinating app
    /// lifecycle transitions around write-capable wallet calls.
    #[uniffi::constructor]
    pub fn new(file_path: String) -> Result<Arc<Self>, FfiError> {
        let config = sqlite_config(file_path)?;
        let rt = crate::runtime::RuntimeGuard::new().map_err(FfiError::internal)?;
        let db = rt
            .block_on(async move { CdkWalletSqliteDatabase::new(config).await })
            .map_err(FfiError::internal)?;
        Ok(Arc::new(Self {
            inner: FfiWalletDatabaseWrapper::new(db),
            _runtime: rt,
        }))
    }

    /// Create an in-memory database
    #[uniffi::constructor]
    pub fn new_in_memory() -> Result<Arc<Self>, FfiError> {
        let rt = crate::runtime::RuntimeGuard::new().map_err(FfiError::internal)?;
        let db = rt
            .block_on(async move { cdk_sqlite::wallet::memory::empty().await })
            .map_err(FfiError::internal)?;
        Ok(Arc::new(Self {
            inner: FfiWalletDatabaseWrapper::new(db),
            _runtime: rt,
        }))
    }
}

fn sqlite_config(file_path: String) -> Result<SqliteConfig, FfiError> {
    if file_path.contains(":memory:") {
        return Ok(SqliteConfig::memory());
    }

    SqliteConfig::file(file_path).map_err(FfiError::internal)
}

#[uniffi::export(async_runtime = "tokio")]
impl WalletSqliteDatabase {
    /// Close this database and all wallets sharing it after stopping their operations.
    ///
    /// Waits for pending database operations and cleanup. Await this before copying
    /// the database file; dropping a handle alone does not wait for closure.
    pub async fn close(&self) -> Result<(), FfiError> {
        self.inner.inner().close().await.map_err(FfiError::database)
    }
}

// Use macro to implement WalletDatabase trait - delegates all methods to inner
crate::impl_ffi_wallet_database!(WalletSqliteDatabase);
