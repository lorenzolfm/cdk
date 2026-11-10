//! SQLite Mint

use cdk_sql_common::mint::SQLMintAuthDatabase;
use cdk_sql_common::SQLMintDatabase;

use crate::common::SqliteConnectionManager;

pub mod memory;

/// Mint SQLite implementation with rusqlite
pub type MintSqliteDatabase = SQLMintDatabase<SqliteConnectionManager>;

/// Mint Auth database with rusqlite
pub type MintSqliteAuthDatabase = SQLMintAuthDatabase<SqliteConnectionManager>;

#[cfg(test)]
mod test {
    use std::fs::remove_file;
    use std::str::FromStr;
    use std::sync::Arc;
    use std::time::Duration;

    use cdk_common::database::{self, MintAuthDatabase};
    use cdk_common::secret::Secret;
    use cdk_common::{mint_db_test, AuthProof, Id, SecretKey, State};
    use cdk_sql_common::database::ConnectionWithTransaction;
    use cdk_sql_common::pool::Pool;
    use cdk_sql_common::stmt::query;

    use super::*;
    use crate::common::Config;

    async fn provide_db(_test_name: String) -> MintSqliteDatabase {
        memory::empty().await.unwrap()
    }

    mint_db_test!(provide_db);

    #[tokio::test]
    async fn kvstore_compare_and_swap() {
        cdk_common::database::mint::test::kvstore_compare_and_swap(
            provide_db("test_kvstore_compare_and_swap".to_owned()).await,
        )
        .await;
    }

    #[tokio::test]
    async fn bug_opening_relative_path() {
        let config: Config = "test.db".into();

        let pool = Pool::<SqliteConnectionManager>::new(config);
        let db = pool.get().await;
        assert!(db.is_ok());
        let _ = remove_file("test.db");
    }

    #[tokio::test]
    async fn exhausted_in_memory_pool_times_out() {
        let config: Config = ":memory:".into();
        let pool = Pool::<SqliteConnectionManager>::new(config);

        let _conn = pool.get().await.expect("valid connection");
        let result = pool.get_timeout(Duration::from_millis(10)).await;

        assert!(matches!(result, Err(cdk_sql_common::pool::Error::Timeout)));
    }

    #[tokio::test]
    async fn cloned_config_creates_independent_in_memory_databases() {
        let config: Config = ":memory:".into();
        let _first = MintSqliteDatabase::new(config.clone())
            .await
            .expect("first database");
        let _second = MintSqliteDatabase::new(config)
            .await
            .expect("independent second database");
    }

    #[test]
    fn discarded_in_memory_connection_is_not_replaced() {
        let config: Config = ":memory:".into();
        let pool = Pool::<SqliteConnectionManager>::new(config);
        let runtime = || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
        };

        // Dropping the runtime drops the unpolled rollback task, which discards
        // the connection and with it the in-memory database.
        let first = runtime();
        first.block_on(async {
            let conn = pool.get().await.expect("valid connection");
            let tx = ConnectionWithTransaction::new(conn)
                .await
                .expect("transaction");
            drop(tx);
        });
        drop(first);

        runtime().block_on(async {
            assert!(matches!(
                pool.get().await,
                Err(cdk_sql_common::pool::Error::Resource(_))
            ));
        });
    }

    async fn spend_auth_proof(
        db: Arc<MintSqliteAuthDatabase>,
        proof: AuthProof,
    ) -> Result<(), database::Error> {
        let mut tx = db.as_ref().begin_transaction().await?;
        tx.add_proof(proof).await?;
        tx.commit().await
    }

    #[tokio::test]
    async fn concurrent_blind_auth_proof_spend_allows_one_request() {
        let path = std::env::temp_dir().join(format!(
            "cdk-blind-auth-replay-{}.sqlite",
            uuid::Uuid::new_v4()
        ));

        #[cfg(not(feature = "sqlcipher"))]
        let db = Arc::new(
            MintSqliteAuthDatabase::new(&path)
                .await
                .expect("auth database"),
        );
        #[cfg(feature = "sqlcipher")]
        let db = Arc::new(
            MintSqliteAuthDatabase::new((path.clone(), "test".to_owned()))
                .await
                .expect("auth database"),
        );

        let proof = AuthProof {
            keyset_id: Id::from_str("00916bbf7ef91a36").expect("valid keyset id"),
            secret: Secret::generate(),
            c: SecretKey::generate().public_key(),
            dleq: None,
        };
        let y = proof.y().expect("proof y");

        let (first, second) = tokio::join!(
            spend_auth_proof(db.clone(), proof.clone()),
            spend_auth_proof(db.clone(), proof)
        );

        assert!(matches!(
            (&first, &second),
            (Ok(()), Err(database::Error::Duplicate)) | (Err(database::Error::Duplicate), Ok(()))
        ));
        assert_eq!(
            db.get_proofs_states(&[y]).await.expect("proof state"),
            vec![Some(State::Spent)]
        );

        drop(db);
        remove_file(path).expect("remove auth database");
    }

    #[tokio::test]
    async fn open_legacy_and_migrate() {
        let file = format!(
            "{}/db.sqlite",
            std::env::temp_dir().to_str().unwrap_or_default()
        );

        {
            let _ = remove_file(&file);
            #[cfg(not(feature = "sqlcipher"))]
            let config: Config = file.as_str().into();
            #[cfg(feature = "sqlcipher")]
            let config: Config = (file.as_str(), "test").into();

            let pool = Pool::<SqliteConnectionManager>::new(config);

            let conn = pool.get().await.expect("valid connection");

            query(include_str!("../../tests/legacy-sqlx.sql"))
                .expect("query")
                .execute(&*conn)
                .await
                .expect("create former db failed");
        }

        #[cfg(not(feature = "sqlcipher"))]
        let conn = MintSqliteDatabase::new(file.as_str()).await;

        #[cfg(feature = "sqlcipher")]
        let conn = MintSqliteDatabase::new((file.as_str(), "test")).await;

        assert!(conn.is_ok(), "Failed with {:?}", conn.unwrap_err());

        let _ = remove_file(&file);
    }

    #[tokio::test]
    async fn keyset_ledger_migration_carries_over_keyset_amounts() {
        use cdk_common::database::{MintProofsDatabase, MintSignaturesDatabase};
        use cdk_common::Amount;

        let path = std::env::temp_dir().join(format!(
            "cdk-keyset-ledger-migration-{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let file = path.to_str().expect("utf-8 temp path");
        #[cfg(not(feature = "sqlcipher"))]
        let open = || MintSqliteDatabase::new(file);
        #[cfg(feature = "sqlcipher")]
        let open = || MintSqliteDatabase::new((file, "test"));

        drop(open().await.expect("migrated database"));

        // Roll the schema back to before the ledger migration and store totals
        // the way the counter table held them.
        {
            #[cfg(not(feature = "sqlcipher"))]
            let config: Config = file.into();
            #[cfg(feature = "sqlcipher")]
            let config: Config = (file, "test").into();
            let pool = Pool::<SqliteConnectionManager>::new(config);
            let conn = pool.get().await.expect("valid connection");
            query(
                r#"
                DROP TABLE keyset_ledger;
                CREATE TABLE keyset_amounts (
                    keyset_id TEXT PRIMARY KEY NOT NULL,
                    total_issued INTEGER NOT NULL DEFAULT 0,
                    total_redeemed INTEGER NOT NULL DEFAULT 0,
                    fee_collected INTEGER NOT NULL DEFAULT 0
                );
                INSERT INTO keyset_amounts (keyset_id, total_issued, total_redeemed, fee_collected)
                VALUES
                    ('00916bbf7ef91a36', 1000, 600, 3),
                    ('00916bbf7ef91a37', 50, 0, 0),
                    ('00916bbf7ef91a38', 0, 0, 0);
                DELETE FROM migrations
                WHERE name = '20261110000000_replace_keyset_amounts_with_ledger.sql';
                "#,
            )
            .expect("query")
            .batch(&*conn)
            .await
            .expect("restore counter table");
        }

        let db = open().await.expect("reapply ledger migration");

        let first = Id::from_str("00916bbf7ef91a36").expect("valid keyset id");
        let second = Id::from_str("00916bbf7ef91a37").expect("valid keyset id");
        let issued = db.get_total_issued().await.expect("total issued");
        let redeemed = db.get_total_redeemed().await.expect("total redeemed");
        assert_eq!(
            issued,
            [(first, Amount::from(1000)), (second, Amount::from(50))].into()
        );
        assert_eq!(redeemed, [(first, Amount::from(600))].into());

        drop(db);
        remove_file(path).expect("remove database");
    }
}
