//! Queries that lock rows for the rest of the transaction

use std::str::FromStr;

use async_trait::async_trait;
use cdk_common::database::mint::SagaTransaction;
use cdk_common::database::{self, Error, MintLockingTransaction};
use cdk_common::mint::{self, MintQuote, Operation, ProofsWithState};
use cdk_common::payment::PaymentIdentifier;
use cdk_common::quote_id::QuoteId;
use cdk_common::util::unix_time;
use cdk_common::{Proof, Proofs, PublicKey, State};
use tracing::instrument;

use super::proofs::sql_row_to_proof_with_state;
use super::quotes::{
    get_melt_quote_inner, get_melt_quotes_by_request_lookup_id_inner,
    get_mint_quote_by_request_inner, get_mint_quote_by_request_lookup_id_inner,
    get_mint_quote_inner, get_mint_quotes_inner, lock_melt_quote_and_related_inner,
};
use super::SQLTransaction;
use crate::column_as_string;
use crate::pool::DatabasePool;
use crate::stmt::query;

#[async_trait]
impl<RM> MintLockingTransaction for SQLTransaction<RM>
where
    RM: DatabasePool + 'static,
{
    type Err = Error;

    async fn lock_mint_quote(
        &mut self,
        quote_id: &QuoteId,
    ) -> Result<Option<MintQuote>, Self::Err> {
        get_mint_quote_inner(&self.inner, quote_id, true).await
    }

    async fn lock_mint_quotes_by_ids(
        &mut self,
        quote_ids: &[QuoteId],
    ) -> Result<Vec<Option<MintQuote>>, Self::Err> {
        get_mint_quotes_inner(&self.inner, quote_ids, true).await
    }

    #[instrument(skip_all)]
    async fn insert_mint_quote(&mut self, quote: &MintQuote) -> Result<(), Self::Err> {
        query(
            r#"
                INSERT INTO mint_quote (
                    id, amount, unit, request, expiry, request_lookup_id, pubkey,
                    created_time, updated_at, last_checked, payment_method,
                    request_lookup_id_kind, extra_json
                )
                VALUES (
                    :id, :amount, :unit, :request, :expiry, :request_lookup_id, :pubkey,
                    :created_time, :updated_at, :last_checked, :payment_method,
                    :request_lookup_id_kind, :extra_json
                )
            "#,
        )?
        .bind("id", quote.id.to_string())
        .bind(
            "amount",
            quote
                .amount
                .clone()
                .map(|a| i64::try_from(a.to_u64()))
                .transpose()?,
        )
        .bind("unit", quote.unit.to_string())
        .bind("request", quote.request.clone())
        .bind("expiry", i64::try_from(quote.expiry)?)
        .bind("request_lookup_id", quote.request_lookup_id.to_string())
        .bind("pubkey", quote.pubkey.map(|p| p.to_string()))
        .bind("created_time", i64::try_from(quote.created_time)?)
        .bind("updated_at", i64::try_from(quote.updated_at())?)
        .bind("last_checked", i64::try_from(quote.last_checked())?)
        .bind("payment_method", quote.payment_method.to_string())
        .bind("request_lookup_id_kind", quote.request_lookup_id.kind())
        .bind(
            "extra_json",
            quote.extra_json.as_ref().map(|v| v.to_string()),
        )
        .execute(&self.inner)
        .await?;

        Ok(())
    }

    async fn lock_melt_quote(
        &mut self,
        quote_id: &QuoteId,
    ) -> Result<Option<mint::MeltQuote>, Self::Err> {
        get_melt_quote_inner(&self.inner, quote_id, true).await
    }

    async fn lock_melt_quotes_by_request_lookup_id(
        &mut self,
        request_lookup_id: &PaymentIdentifier,
    ) -> Result<Vec<mint::MeltQuote>, Self::Err> {
        get_melt_quotes_by_request_lookup_id_inner(&self.inner, request_lookup_id, true).await
    }

    async fn lock_related_melt_quotes(
        &mut self,
        quote_id: &QuoteId,
    ) -> Result<Vec<mint::MeltQuote>, Self::Err> {
        lock_melt_quote_and_related_inner(&self.inner, quote_id).await
    }

    async fn lock_mint_quote_by_request(
        &mut self,
        request: &str,
    ) -> Result<Option<MintQuote>, Self::Err> {
        get_mint_quote_by_request_inner(&self.inner, request, true).await
    }

    async fn lock_mint_quote_by_request_lookup_id(
        &mut self,
        request_lookup_id: &PaymentIdentifier,
    ) -> Result<Option<MintQuote>, Self::Err> {
        get_mint_quote_by_request_lookup_id_inner(&self.inner, request_lookup_id, true).await
    }

    /// Adds proofs to the database with initial state `Unspent`.
    ///
    /// This method first checks if any of the proofs already exist in the database.
    /// If a proof exists and is spent, returns [`Error::AttemptUpdateSpentProof`].
    /// If a proof exists in any other state, returns [`Error::Duplicate`].
    async fn insert_proofs(
        &mut self,
        proofs: &Proofs,
        quote_id: Option<QuoteId>,
        operation: &Operation,
    ) -> Result<(), Self::Err> {
        let current_time = unix_time();

        let mut ordered_proofs = proofs
            .iter()
            .map(|proof| Ok((proof.y()?.to_bytes().to_vec(), proof)))
            .collect::<Result<Vec<_>, Error>>()?;
        ordered_proofs.sort_unstable_by(|(left_y, _), (right_y, _)| left_y.cmp(right_y));

        // Check any previous proof, this query should return None in order to proceed storing
        // Any result here would error
        match query(r#"SELECT state FROM proof WHERE y IN (:ys) ORDER BY y LIMIT 1 FOR UPDATE"#)?
            .bind_vec(
                "ys",
                ordered_proofs.iter().map(|(y, _)| y.clone()).collect(),
            )?
            .pluck(&self.inner)
            .await?
            .map(|state| Ok::<_, Error>(column_as_string!(&state, State::from_str)))
            .transpose()?
        {
            Some(State::Spent) => Err(database::Error::AttemptUpdateSpentProof),
            Some(_) => Err(database::Error::Duplicate),
            None => Ok(()), // no previous record
        }?;

        for (y, proof) in ordered_proofs {
            query(
                r#"
                  INSERT INTO proof
                  (y, amount, keyset_id, secret, c, witness, state, quote_id, created_time, operation_kind, operation_id)
                  VALUES
                  (:y, :amount, :keyset_id, :secret, :c, :witness, :state, :quote_id, :created_time, :operation_kind, :operation_id)
                  "#,
            )?
            .bind("y", y)
            .bind("amount", i64::try_from(proof.amount.to_u64())?)
            .bind("keyset_id", proof.keyset_id.to_string())
            .bind("secret", proof.secret.to_string())
            .bind("c", proof.c.to_bytes().to_vec())
            .bind(
                "witness",
                proof.witness.clone().and_then(|w| serde_json::to_string(&w).inspect_err(|e| tracing::error!("Failed to serialize witness: {:?}", e)).ok()),
            )
            .bind("state", "UNSPENT".to_string())
            .bind("quote_id", quote_id.clone().map(|q| q.to_string()))
            .bind("created_time", i64::try_from(current_time)?)
            .bind("operation_kind", operation.kind().to_string())
            .bind("operation_id", operation.id().to_string())
            .execute(&self.inner)
            .await?;
        }

        Ok(())
    }

    async fn lock_proofs(&mut self, ys: &[PublicKey]) -> Result<ProofsWithState, Self::Err> {
        let rows = query(
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
                 y IN (:ys)
             ORDER BY y
             FOR UPDATE
             "#,
        )?
        .bind_vec("ys", ys.iter().map(|y| y.to_bytes().to_vec()).collect())?
        .fetch_all(&self.inner)
        .await?;

        if rows.is_empty() || rows.len() != ys.len() {
            return Err(database::Error::ProofNotFound);
        }

        let results: Vec<(Proof, State)> = rows
            .into_iter()
            .map(sql_row_to_proof_with_state)
            .collect::<Result<Vec<_>, _>>()?;

        let mut proofs = Vec::with_capacity(results.len());
        let mut first_state: Option<State> = None;

        for (proof, state) in results {
            if let Some(first) = first_state {
                if first != state {
                    return Err(database::Error::Internal(
                        "Proofs have inconsistent states".to_string(),
                    ));
                }
            } else {
                first_state = Some(state);
            }

            proofs.push(proof);
        }

        let state = first_state.unwrap_or(State::Unspent);
        Ok(ProofsWithState::new(proofs, state))
    }

    async fn lock_saga(
        &mut self,
        operation_id: &uuid::Uuid,
    ) -> Result<Option<mint::Saga>, Self::Err> {
        self.get_saga(operation_id).await
    }
}
