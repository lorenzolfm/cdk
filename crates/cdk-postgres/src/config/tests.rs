use tokio_postgres::config::SslMode as PgSslMode;

use super::*;

#[test]
fn keyword_credentials_and_application_names_are_preserved() {
    for (input, password) in [
        ("password='sslmode=verify-full'", "sslmode=verify-full"),
        (
            "password='a  schema=private sslmode=disable  b'",
            "a  schema=private sslmode=disable  b",
        ),
        (r"password=a\ b\ schema=private", "a b schema=private"),
        (r"password='a\'b\\c'", "a'b\\c"),
    ] {
        let config = PgConfig::from(
            format!(
                "host=localhost {input} application_name='sslmode=verify-ca' \
                 sslmode='require' schema='my schema'"
            )
            .as_str(),
        );
        let (driver, tls) = config.driver_config().expect("valid configuration");
        assert_eq!(driver.get_password(), Some(password.as_bytes()));
        assert_eq!(driver.get_application_name(), Some("sslmode=verify-ca"));
        assert_eq!(driver.get_ssl_mode(), PgSslMode::Require);
        assert!(matches!(tls, SslMode::NativeTls(_)));
        assert_eq!(config.schema.as_deref(), Some("my schema"));
    }
}

#[test]
fn uri_credentials_and_encoded_parameters_are_preserved() {
    for uri in [
        "postgres://user:sslmode=verify-full@localhost/db?sslmode=verify%2Dfull&schema=a%20b%2Bc",
        "postgresql://user:sslmode%3Dverify-full@localhost/db?%73slmode=verify-full&schema=a%20b%2Bc",
    ] {
        let config = PgConfig::from(uri);
        let (driver, _) = config.driver_config().expect("valid configuration");
        assert_eq!(driver.get_password(), Some(b"sslmode=verify-full".as_slice()));
        assert_eq!(driver.get_ssl_mode(), PgSslMode::Require);
        assert_eq!(config.schema.as_deref(), Some("a b+c"));
    }
    let config = PgConfig::from("postgres://localhost/db? schema='legacy schema'");
    assert!(config.driver_config().is_ok());
    assert_eq!(config.schema.as_deref(), Some("legacy schema"));
}

#[test]
fn tls_precedence_and_duplicate_parameters_match_driver_behavior() {
    let config = PgConfig::new(
        "host=localhost sslmode = 'prefer' sslmode=require",
        Some("disable"),
        None,
        None,
    );
    let (driver, tls) = config.driver_config().expect("valid configuration");
    assert_eq!(driver.get_ssl_mode(), PgSslMode::Disable);
    assert!(matches!(tls, SslMode::NoTls(_)));
    let config = PgConfig::from("host=localhost sslmode=disable sslmode='require'");
    let (driver, _) = config.driver_config().expect("valid configuration");
    assert_eq!(driver.get_ssl_mode(), PgSslMode::Require);
}

#[test]
fn invalid_tls_mode_in_the_connection_string_reports_the_tls_error() {
    for input in [
        "host=localhost sslmode=requre",          // typos: ignore
        "postgres://localhost/db?sslmode=requre", // typos: ignore
    ] {
        for explicit in [None, Some("require")] {
            let error = PgConfig::new(input, explicit, None, None)
                .validate()
                .expect_err("invalid TLS mode");
            assert_eq!(error.to_string(), "Invalid PostgreSQL TLS mode", "{input}");
        }
    }
}

#[test]
fn invalid_extensions_cannot_inject_driver_parameters_or_leak_credentials() {
    for input in [
        "password=secret schema='unterminated",
        "password=secret sslmode='disable user=attacker'",
        "postgres://user:secret@localhost/db?schema=%FF",
        "password=secret schema=",
        "postgres://user:secret@localhost/db sslmode=disable",
        "password='secret",
    ] {
        let config = PgConfig::new(input, Some("disable"), None, None);
        let error = config.driver_config().expect_err("invalid configuration");
        assert!(!format!("{error:?}").contains("secret"), "{input}");
        assert!(!error.to_string().contains("secret"), "{input}");
        assert!(!format!("{config:?}").contains("secret"), "{input}");
    }
}

#[test]
fn keyword_password_decoding_matches_the_driver_at_boundaries() {
    for input in [
        r"password=trailing\",
        r"password=escaped\ space",
        "password=''",
        "password=' Unicode 密碼  '",
        "password='one' application_name=two",
    ] {
        let original: tokio_postgres::Config = input.parse().expect("driver parses input");
        let (adapted, _) = PgConfig::from(input)
            .driver_config()
            .expect("valid configuration");
        assert_eq!(adapted.get_password(), original.get_password());
        assert_eq!(
            adapted.get_application_name(),
            original.get_application_name()
        );
    }
}

#[test]
fn quoted_or_escaped_schema_records_the_legacy_literal_name() {
    for (input, schema, legacy) in [
        ("host=localhost schema=mint", "mint", None),
        ("host=localhost schema='mint'", "mint", Some("'mint'")),
        (r"host=localhost schema=mi\nt", "mint", Some(r"mi\nt")),
        // Earlier releases read each `""` inside the quoted identifier as `"`.
        (r#"host=localhost schema=a""b"#, r#"a""b"#, Some(r#"a"b"#)),
        (
            r#"host=localhost schema='a""b'"#,
            r#"a""b"#,
            Some(r#"'a"b'"#),
        ),
        (r#"host=localhost schema=a"b"#, r#"a"b"#, None),
        (
            "postgres://localhost/db schema='mint'",
            "mint",
            Some("'mint'"),
        ),
        // Earlier releases rejected schema query parameters, so no legacy name.
        ("postgres://localhost/db?schema=%27mint%27", "'mint'", None),
    ] {
        let config = PgConfig::from(input);
        assert_eq!(config.schema.as_deref(), Some(schema), "{input}");
        assert_eq!(config.legacy_schema.as_deref(), legacy, "{input}");
    }
}
