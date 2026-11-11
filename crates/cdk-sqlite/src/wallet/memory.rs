//! Wallet in memory database

use cdk_common::database::Error;

use super::WalletSqliteDatabase;
use crate::SqliteConfig;

/// Creates a new in-memory [`WalletSqliteDatabase`] instance
pub async fn empty() -> Result<WalletSqliteDatabase, Error> {
    WalletSqliteDatabase::new(SqliteConfig::memory()).await
}
