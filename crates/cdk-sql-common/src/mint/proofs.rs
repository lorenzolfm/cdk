//! Proofs database implementation

use std::collections::HashMap;
use std::str::FromStr;

use async_trait::async_trait;
use cdk_common::database::mint::Acquired;
use cdk_common::database::{self, Error, MintProofsDatabase};
use cdk_common::mint::ProofsWithState;
use cdk_common::nut00::ProofsMethods;
use cdk_common::quote_id::QuoteId;
use cdk_common::secret::Secret;
use cdk_common::{Amount, Id, Proof, Proofs, PublicKey, State};

use super::{SQLMintDatabase, SQLTransaction};
use crate::database::DatabaseExecutor;
use crate::pool::DatabasePool;
use crate::stmt::{query, Column};
use crate::{column_as_nullable_string, column_as_number, column_as_string, unpack_into};

pub(super) async fn get_current_states<C>(
    conn: &C,
    ys: &[PublicKey],
    for_update: bool,
) -> Result<HashMap<PublicKey, State>, Error>
where
    C: DatabaseExecutor + Send + Sync,
{
    let for_update_clause = if for_update { "FOR UPDATE" } else { "" };

    query(&format!(
        r#"SELECT y, state FROM proof WHERE y IN (:ys) ORDER BY y {}"#,
        for_update_clause
    ))?
    .bind_vec("ys", ys.iter().map(|y| y.to_bytes().to_vec()).collect())?
    .fetch_all(conn)
    .await?
    .into_iter()
    .map(|row| {
        Ok((
            column_as_string!(&row[0], PublicKey::from_hex, PublicKey::from_slice),
            column_as_string!(&row[1], State::from_str),
        ))
    })
    .collect::<Result<HashMap<_, _>, _>>()
}

pub(super) fn sql_row_to_proof(row: Vec<Column>) -> Result<Proof, Error> {
    unpack_into!(
        let (
            amount,
            keyset_id,
            secret,
            c,
            witness
        ) = row
    );

    let amount: u64 = column_as_number!(amount);
    Ok(Proof {
        amount: Amount::from(amount),
        keyset_id: column_as_string!(keyset_id, Id::from_str),
        secret: column_as_string!(secret, Secret::from_str),
        c: column_as_string!(c, PublicKey::from_hex, PublicKey::from_slice),
        witness: column_as_nullable_string!(witness).and_then(|w| serde_json::from_str(&w).ok()),
        dleq: None,
        p2pk_e: None,
    })
}

pub(super) fn sql_row_to_proof_with_state(row: Vec<Column>) -> Result<(Proof, State), Error> {
    unpack_into!(
        let (
            keyset_id, amount, secret, c, witness, state
        ) = row
    );

    let amount: u64 = column_as_number!(amount);
    let state = column_as_nullable_string!(state)
        .and_then(|s| State::from_str(&s).ok())
        .unwrap_or(State::Pending);

    Ok((
        Proof {
            amount: Amount::from(amount),
            keyset_id: column_as_string!(keyset_id, Id::from_str, Id::from_bytes),
            secret: column_as_string!(secret, Secret::from_str),
            c: column_as_string!(c, PublicKey::from_hex, PublicKey::from_slice),
            witness: column_as_nullable_string!(witness)
                .and_then(|w| serde_json::from_str(&w).ok()),
            dleq: None,
            p2pk_e: None,
        },
        state,
    ))
}

pub(super) fn sql_row_to_hashmap_amount(row: Vec<Column>) -> Result<(Id, Amount), Error> {
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

#[async_trait]
impl<RM> database::MintProofsTransaction for SQLTransaction<RM>
where
    RM: DatabasePool + 'static,
{
    type Err = Error;

    /// Updates all proofs to the given state in the database.
    ///
    /// Also updates the `state` field on the [`ProofsWithState`] wrapper to reflect
    /// the new state after the database update succeeds.
    ///
    /// When the new state is `Spent`, this method also updates the `keyset_amounts`
    /// table to track the total redeemed amount per keyset for analytics purposes.
    ///
    /// # Prerequisites
    ///
    /// The proofs must have been previously acquired via `add_proofs`
    /// or `get_proofs` to ensure they are locked within the current transaction.
    async fn update_proofs_state(
        &mut self,
        proofs: &mut Acquired<ProofsWithState>,
        new_state: State,
    ) -> Result<(), Self::Err> {
        let ys = proofs.ys()?;

        query(r#"UPDATE proof SET state = :new_state WHERE y IN (:ys)"#)?
            .bind("new_state", new_state.to_string())
            .bind_vec("ys", ys.iter().map(|y| y.to_bytes().to_vec()).collect())?
            .execute(&self.inner)
            .await?;

        if new_state == State::Spent {
            query(
                    r#"
                    INSERT INTO keyset_amounts (keyset_id, total_issued, total_redeemed)
                    SELECT keyset_id, 0, COALESCE(SUM(amount), 0)
                    FROM proof
                    WHERE y IN (:ys)
                    GROUP BY keyset_id
                    ORDER BY keyset_id
                    ON CONFLICT (keyset_id)
                    DO UPDATE SET total_redeemed = keyset_amounts.total_redeemed + EXCLUDED.total_redeemed
                    "#,
                )?
                .bind_vec("ys", ys.iter().map(|y| y.to_bytes().to_vec()).collect())?
                .execute(&self.inner)
                .await?;
        }

        proofs.state = new_state;

        Ok(())
    }

    async fn remove_proofs(
        &mut self,
        ys: &[PublicKey],
        _quote_id: Option<QuoteId>,
    ) -> Result<(), Self::Err> {
        // Acquire all rows that can be deleted in primary-key order. This keeps
        // concurrent proof cleanup and finalization from locking batches in
        // different orders.
        query(
            r#"
            SELECT y
            FROM proof
            WHERE y IN (:ys) AND state NOT IN (:exclude_state)
            ORDER BY y
            FOR UPDATE
            "#,
        )?
        .bind_vec("ys", ys.iter().map(|y| y.to_bytes().to_vec()).collect())?
        .bind_vec("exclude_state", vec![State::Spent.to_string()])?
        .fetch_all(&self.inner)
        .await?;

        let total_deleted = query(
            r#"
            DELETE FROM proof WHERE y IN (:ys) AND state NOT IN (:exclude_state)
            "#,
        )?
        .bind_vec("ys", ys.iter().map(|y| y.to_bytes().to_vec()).collect())?
        .bind_vec("exclude_state", vec![State::Spent.to_string()])?
        .execute(&self.inner)
        .await?;

        if total_deleted != ys.len() {
            // Query current states to provide detailed logging
            let current_states = get_current_states(&self.inner, ys, true).await?;

            let missing_count = ys.len() - current_states.len();
            let spent_count = current_states
                .values()
                .filter(|s| **s == State::Spent)
                .count();

            if missing_count > 0 {
                tracing::warn!(
                    "remove_proofs: {} of {} proofs do not exist in database (already removed?)",
                    missing_count,
                    ys.len()
                );
            }

            if spent_count > 0 {
                tracing::warn!(
                    "remove_proofs: {} of {} proofs are in Spent state and cannot be removed",
                    spent_count,
                    ys.len()
                );
            }

            tracing::debug!(
                "remove_proofs details: requested={}, deleted={}, missing={}, spent={}",
                ys.len(),
                total_deleted,
                missing_count,
                spent_count
            );

            return Err(Self::Err::AttemptRemoveSpentProof);
        }

        Ok(())
    }

    async fn get_proof_ys_by_quote_id(
        &mut self,
        quote_id: &QuoteId,
    ) -> Result<Vec<PublicKey>, Self::Err> {
        Ok(query(
            r#"
            SELECT
                amount,
                keyset_id,
                secret,
                c,
                witness
            FROM
                proof
            WHERE
                quote_id = :quote_id
            ORDER BY y
            FOR UPDATE
            "#,
        )?
        .bind("quote_id", quote_id.to_string())
        .fetch_all(&self.inner)
        .await?
        .into_iter()
        .map(sql_row_to_proof)
        .collect::<Result<Vec<Proof>, _>>()?
        .ys()?)
    }

    async fn get_proof_ys_by_operation_id(
        &mut self,
        operation_id: &uuid::Uuid,
    ) -> Result<Vec<PublicKey>, Self::Err> {
        Ok(query(
            r#"
            SELECT
                y
            FROM
                proof
            WHERE
                operation_id = :operation_id
            "#,
        )?
        .bind("operation_id", operation_id.to_string())
        .fetch_all(&self.inner)
        .await?
        .into_iter()
        .map(|row| -> Result<PublicKey, Error> {
            Ok(column_as_string!(
                &row[0],
                PublicKey::from_hex,
                PublicKey::from_slice
            ))
        })
        .collect::<Result<Vec<_>, _>>()?)
    }
}

#[async_trait]
impl<RM> MintProofsDatabase for SQLMintDatabase<RM>
where
    RM: DatabasePool + 'static,
{
    type Err = Error;

    async fn get_proofs_by_ys(&self, ys: &[PublicKey]) -> Result<Vec<Option<Proof>>, Self::Err> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| Error::Database(Box::new(e)))?;
        let mut proofs = query(
            r#"
            SELECT
                amount,
                keyset_id,
                secret,
                c,
                witness,
                y
            FROM
                proof
            WHERE
                y IN (:ys)
            "#,
        )?
        .bind_vec("ys", ys.iter().map(|y| y.to_bytes().to_vec()).collect())?
        .fetch_all(&*conn)
        .await?
        .into_iter()
        .map(|mut row| {
            Ok((
                column_as_string!(
                    row.pop().ok_or(Error::InvalidDbResponse)?,
                    PublicKey::from_hex,
                    PublicKey::from_slice
                ),
                sql_row_to_proof(row)?,
            ))
        })
        .collect::<Result<HashMap<_, _>, Error>>()?;

        Ok(ys.iter().map(|y| proofs.remove(y)).collect())
    }

    async fn get_proof_ys_by_quote_id(
        &self,
        quote_id: &QuoteId,
    ) -> Result<Vec<PublicKey>, Self::Err> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| Error::Database(Box::new(e)))?;
        Ok(query(
            r#"
            SELECT
                amount,
                keyset_id,
                secret,
                c,
                witness
            FROM
                proof
            WHERE
                quote_id = :quote_id
            "#,
        )?
        .bind("quote_id", quote_id.to_string())
        .fetch_all(&*conn)
        .await?
        .into_iter()
        .map(sql_row_to_proof)
        .collect::<Result<Vec<Proof>, _>>()?
        .ys()?)
    }

    async fn get_proofs_states(&self, ys: &[PublicKey]) -> Result<Vec<Option<State>>, Self::Err> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| Error::Database(Box::new(e)))?;
        let current_states = get_current_states(&*conn, ys, false).await?;

        Ok(ys.iter().map(|y| current_states.get(y).copied()).collect())
    }

    async fn get_proofs_by_keyset_id(
        &self,
        keyset_id: &Id,
    ) -> Result<(Proofs, Vec<Option<State>>), Self::Err> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| Error::Database(Box::new(e)))?;

        let (proofs, states): (Vec<Proof>, Vec<State>) = query(
            r#"
            SELECT
               keyset_id,
               amount,
               secret,
               c,
               witness,
               state
            FROM
                proof
            WHERE
                keyset_id=:keyset_id
            "#,
        )?
        .bind("keyset_id", keyset_id.to_string())
        .fetch_all(&*conn)
        .await?
        .into_iter()
        .map(sql_row_to_proof_with_state)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .unzip();

        Ok((proofs, states.into_iter().map(Some).collect()))
    }

    /// Get total proofs redeemed by keyset id
    async fn get_total_redeemed(&self) -> Result<HashMap<Id, Amount>, Self::Err> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| Error::Database(Box::new(e)))?;
        query(
            r#"
            SELECT
                keyset_id,
                total_redeemed as amount
            FROM
                keyset_amounts
        "#,
        )?
        .fetch_all(&*conn)
        .await?
        .into_iter()
        .map(sql_row_to_hashmap_amount)
        .collect()
    }

    async fn get_proof_ys_by_operation_id(
        &self,
        operation_id: &uuid::Uuid,
    ) -> Result<Vec<PublicKey>, Self::Err> {
        let conn = self
            .pool
            .get()
            .await
            .map_err(|e| Error::Database(Box::new(e)))?;
        query(
            r#"
            SELECT
                y
            FROM
                proof
            WHERE
                operation_id = :operation_id
            "#,
        )?
        .bind("operation_id", operation_id.to_string())
        .fetch_all(&*conn)
        .await?
        .into_iter()
        .map(|row| -> Result<PublicKey, Error> {
            Ok(column_as_string!(
                &row[0],
                PublicKey::from_hex,
                PublicKey::from_slice
            ))
        })
        .collect::<Result<Vec<_>, _>>()
    }
}
