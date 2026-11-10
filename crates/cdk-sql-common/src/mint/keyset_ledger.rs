//! Append-only ledger of the amounts issued, redeemed and collected as fees per keyset
//!
//! Every write appends a new row instead of updating a shared counter row, so
//! concurrent operations on the same keyset never wait on each other here.
//! Totals are computed on read.

use std::collections::HashMap;
use std::str::FromStr;

use cdk_common::database::Error;
use cdk_common::{Amount, Id};

use crate::database::DatabaseExecutor;
use crate::stmt::{query, Column};
use crate::{column_as_number, column_as_string, unpack_into};

/// What a ledger entry records
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum EntryKind {
    /// Amount signed by the mint
    Issued,
    /// Amount of proofs marked spent
    Redeemed,
    /// Input fees collected
    Fee,
}

impl EntryKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Issued => "issued",
            Self::Redeemed => "redeemed",
            Self::Fee => "fee",
        }
    }
}

/// Append one entry. A zero amount is skipped: the table only stores positive amounts.
pub(super) async fn append<C>(
    conn: &C,
    kind: EntryKind,
    keyset_id: &Id,
    amount: Amount,
) -> Result<(), Error>
where
    C: DatabaseExecutor,
{
    if amount == Amount::ZERO {
        return Ok(());
    }

    query(
        r#"
        INSERT INTO keyset_ledger (keyset_id, kind, amount)
        VALUES (:keyset_id, :kind, :amount)
        "#,
    )?
    .bind("keyset_id", keyset_id.to_string())
    .bind("kind", kind.as_str().to_owned())
    .bind("amount", i64::try_from(amount.to_u64())?)
    .execute(conn)
    .await?;

    Ok(())
}

/// Append one redeemed entry per keyset for the proofs with the given `ys`.
pub(super) async fn append_redeemed_for_proofs<C>(conn: &C, ys: Vec<Vec<u8>>) -> Result<(), Error>
where
    C: DatabaseExecutor,
{
    query(
        r#"
        INSERT INTO keyset_ledger (keyset_id, kind, amount)
        SELECT keyset_id, :kind, SUM(amount)
        FROM proof
        WHERE y IN (:ys)
        GROUP BY keyset_id
        HAVING SUM(amount) > 0
        "#,
    )?
    .bind("kind", EntryKind::Redeemed.as_str().to_owned())
    .bind_vec("ys", ys)?
    .execute(conn)
    .await?;

    Ok(())
}

/// Sum the entries of one kind per keyset. Keysets without entries are absent.
pub(super) async fn totals<C>(conn: &C, kind: EntryKind) -> Result<HashMap<Id, Amount>, Error>
where
    C: DatabaseExecutor,
{
    query(
        r#"
        SELECT keyset_id, CAST(SUM(amount) AS BIGINT) AS amount
        FROM keyset_ledger
        WHERE kind = :kind
        GROUP BY keyset_id
        "#,
    )?
    .bind("kind", kind.as_str().to_owned())
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(sql_row_to_hashmap_amount)
    .collect()
}

fn sql_row_to_hashmap_amount(row: Vec<Column>) -> Result<(Id, Amount), Error> {
    unpack_into!(
        let (
            keyset_id, amount
        ) = row
    );

    let amount: u64 = column_as_number!(amount);
    Ok((
        column_as_string!(keyset_id, Id::from_str, Id::from_bytes),
        Amount::from(amount),
    ))
}
