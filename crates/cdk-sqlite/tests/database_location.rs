//! A file location opens exactly the file it names.
//!
//! These tests change the current directory, so they live in their own test
//! binary and run as a single test.
#![cfg(unix)]

use std::path::{Path, PathBuf};

use cdk_common::database::WalletDatabase;
use cdk_common::mint_url::MintUrl;
use cdk_sqlite::{SqliteConfig, SqliteConfigError, WalletSqliteDatabase};

fn mint_url() -> MintUrl {
    "https://mint.example".parse().expect("mint URL")
}

/// Opens `path`, writes a mint, reopens it and checks the mint is still there.
async fn assert_persists(path: &Path) {
    let db = WalletSqliteDatabase::new(SqliteConfig::file(path).expect("valid path"))
        .await
        .expect("open wallet database");
    db.add_mint(mint_url(), None).await.expect("add mint");
    drop(db);

    let db = WalletSqliteDatabase::new(SqliteConfig::file(path).expect("valid path"))
        .await
        .expect("reopen wallet database");
    assert!(
        db.get_mints()
            .await
            .expect("get mints")
            .contains_key(&mint_url()),
        "{} lost its writes",
        path.display()
    );
}

#[tokio::test]
async fn file_locations_open_the_named_file() {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("cdk-sqlite-location-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).expect("create test directory");
    std::env::set_current_dir(&dir).expect("enter test directory");

    // Names SQLite would otherwise read as in-memory databases or URIs.
    for name in [
        ":memory:",
        "file:wallet.sqlite",
        "file:other.sqlite?mode=memory",
    ] {
        assert_persists(Path::new(name)).await;
        assert!(dir.join(name).is_file(), "{name} was not created");
    }
    assert!(
        !dir.join("wallet.sqlite").exists(),
        "file:wallet.sqlite opened wallet.sqlite"
    );

    assert_persists(&dir.join("wallet:memory:.sqlite")).await;

    {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let non_utf8 = dir.join(OsStr::from_bytes(b"wallet-\xff.sqlite"));
        assert_persists(&non_utf8).await;
        assert!(non_utf8.is_file());
    }

    assert_eq!(
        SqliteConfig::file("").map(|_| ()),
        Err(SqliteConfigError::EmptyPath)
    );

    // A relative path names the file in the directory current when the config
    // is built, even if the process changes directory before opening it.
    let config = SqliteConfig::file("moved.sqlite").expect("valid path");
    let elsewhere = dir.join("elsewhere");
    std::fs::create_dir(&elsewhere).expect("create second directory");
    std::env::set_current_dir(&elsewhere).expect("enter second directory");
    let db = WalletSqliteDatabase::new(config)
        .await
        .expect("open wallet database");
    db.add_mint(mint_url(), None).await.expect("add mint");
    drop(db);
    assert!(
        dir.join("moved.sqlite").is_file(),
        "moved.sqlite not created"
    );
    assert!(
        !elsewhere.join("moved.sqlite").exists(),
        "the directory change moved the database"
    );

    std::env::set_current_dir(std::env::temp_dir()).expect("leave test directory");
    std::fs::remove_dir_all(&dir).expect("remove test directory");
}
