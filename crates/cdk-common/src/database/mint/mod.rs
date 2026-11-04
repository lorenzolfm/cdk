//! CDK Database

use std::collections::HashMap;
use std::ops::DerefMut;

use async_trait::async_trait;
use cashu::quote_id::QuoteId;
use cashu::Amount;

use super::{DbTransactionFinalizer, Error};
use crate::mint::{
    self, MeltQuote, MintKeySetInfo, MintQuote as MintMintQuote, Operation, ProofsWithState,
};
use crate::nuts::{
    BlindSignature, BlindedMessage, CurrencyUnit, Id, MeltQuoteState, Proof, Proofs, PublicKey,
    State,
};
use crate::payment::PaymentIdentifier;

mod auth;
pub mod prelude;

#[cfg(feature = "test")]
pub mod test;

pub use auth::{DynMintAuthDatabase, MintAuthDatabase, MintAuthTransaction};

// Re-export KVStore types from shared module for backward compatibility
pub use super::kvstore::{
    validate_kvstore_params, validate_kvstore_string, KVStore, KVStoreDatabase, KVStoreTransaction,
    KVSTORE_NAMESPACE_KEY_ALPHABET, KVSTORE_NAMESPACE_KEY_MAX_LEN,
};

/// A value wrapper a transaction hands out once it holds the value's row lock.
///
/// Each backend has its own wrapper type and keeps its constructor private, so
/// only the code that ran the locking query can build one.
///
/// `DerefMut` gives transparent access to the value's fields and methods.
pub trait LockedValue<T>: DerefMut<Target = T> + Send {
    /// Consumes the wrapper and returns the value.
    ///
    /// Use this once the value no longer needs to be written through the
    /// transaction, for example to return it after the commit.
    fn into_inner(self) -> T;
}

/// Base of the transaction traits that hand out or take locked values.
///
/// [`QuotesTransaction`], [`ProofsTransaction`] and [`SagaTransaction`] share it
/// so that one transaction has one wrapper type.
///
/// Code that is generic over the backend can write a value the transaction
/// locked, from a future that can move between threads:
///
/// ```
/// use cdk_common::database::mint::prelude::*;
/// use cdk_common::database::{Error, MintTransaction};
/// use cdk_common::mint::MintQuote;
/// use cdk_common::QuoteId;
///
/// async fn write_locked<T>(tx: &mut T, id: &QuoteId) -> Result<Option<MintQuote>, Error>
/// where
///     T: MintTransaction<Error> + Send,
/// {
///     let Some(mut quote) = tx.get_mint_quote(id).await? else {
///         return Ok(None);
///     };
///     tx.update_mint_quote(&mut quote).await?;
///     Ok(Some(quote.into_inner()))
/// }
///
/// fn assert_send<F>(_future: F)
/// where
///     F: Send,
/// {
/// }
///
/// fn write_locked_is_send<T>(tx: &mut T, id: &QuoteId)
/// where
///     T: MintTransaction<Error> + Send,
/// {
///     assert_send(write_locked(tx, id));
/// }
/// ```
///
/// That code can't turn a plain value into a locked one:
///
/// ```compile_fail,E0277
/// use cdk_common::database::mint::prelude::*;
/// use cdk_common::database::{Error, MintTransaction};
/// use cdk_common::mint::MintQuote;
///
/// async fn write_plain<T>(tx: &mut T, quote: MintQuote) -> Result<(), Error>
/// where
///     T: MintTransaction<Error> + Send,
/// {
///     let mut quote: T::Locked<MintQuote> = quote.into();
///     tx.update_mint_quote(&mut quote).await
/// }
/// ```
///
/// or pass a value locked by another backend:
///
/// ```compile_fail,E0308
/// use cdk_common::database::mint::prelude::*;
/// use cdk_common::database::{Error, MintTransaction};
/// use cdk_common::mint::MintQuote;
/// use cdk_common::QuoteId;
///
/// async fn write_across_backends<A, B>(a: &mut A, b: &mut B, id: &QuoteId) -> Result<(), Error>
/// where
///     A: MintTransaction<Error> + Send,
///     B: MintTransaction<Error> + Send,
/// {
///     if let Some(mut quote) = a.get_mint_quote(id).await? {
///         b.update_mint_quote(&mut quote).await?;
///     }
///     Ok(())
/// }
/// ```
pub trait LockingTransaction {
    /// A storage-specific mutable wrapper around a value locked by a transaction.
    ///
    /// Returned by operations that lock rows for update (e.g.,
    /// `SELECT ... FOR UPDATE`) and required by the operations that write them.
    ///
    /// ```ignore
    /// // Lock the row
    /// let mut quote = tx.get_melt_quote(&quote_id).await?.ok_or(Error::UnknownQuote)?;
    ///
    /// // Persist the change
    /// tx.update_melt_quote_state(&mut quote, MeltQuoteState::Paid, None).await?;
    /// ```
    type Locked<T>: LockedValue<T>
    where
        T: Send;
}

/// Information about a melt request stored in the database
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeltRequestInfo {
    /// Total amount of all input proofs in the melt request
    pub inputs_amount: Amount<CurrencyUnit>,
    /// Fee amount associated with the input proofs
    pub inputs_fee: Amount<CurrencyUnit>,
    /// Blinded messages for change outputs
    pub change_outputs: Vec<BlindedMessage>,
}

/// Result of locking a melt quote and all related quotes atomically.
///
/// This struct is returned by [`QuotesTransaction::lock_melt_quote_and_related`]
/// and contains both the target quote and all quotes sharing the same `request_lookup_id`.
#[derive(Debug)]
pub struct LockedMeltQuotes<Q> {
    /// The target quote that was requested, if found
    pub target: Option<Q>,
    /// All quotes sharing the same `request_lookup_id` (including the target)
    pub all_related: Vec<Q>,
}

/// KeysDatabaseWriter
#[async_trait]
pub trait KeysDatabaseTransaction<'a, Error>: DbTransactionFinalizer<Err = Error> {
    /// Add Active Keyset
    async fn set_active_keyset(&mut self, unit: CurrencyUnit, id: Id) -> Result<(), Error>;

    /// Add [`MintKeySetInfo`]
    async fn add_keyset_info(&mut self, keyset: MintKeySetInfo) -> Result<(), Error>;

    /// Allocate the next derivation-path index for a unit.
    ///
    /// Returns `max(derivation_path_index) + 1` for the unit (1 when the unit
    /// has no keysets yet). The transaction holds the global keyset advisory
    /// lock (taken when it begins), so all keyset transactions serialize and two
    /// signatory instances rotating the same unit cannot allocate the same index
    /// and derive divergent keysets. This is the authoritative index source;
    /// callers must not compute it from process-local state.
    async fn next_derivation_index(&mut self, unit: &CurrencyUnit) -> Result<u32, Error>;

    /// Read a unit's keyset infos inside the transaction, under the global
    /// keyset advisory lock the transaction holds.
    ///
    /// Boot-time reactivation uses this to pick a unit's highest-index keyset
    /// and reassign the active pointer as one atomic step. Both happen inside the
    /// transaction, under the global keyset lock, so a peer rotation cannot commit
    /// a higher keyset between the read and the reassignment and get clobbered
    /// back to the older one.
    async fn get_keyset_infos_by_unit(
        &mut self,
        unit: &CurrencyUnit,
    ) -> Result<Vec<MintKeySetInfo>, Error>;

    /// Read all active keyset pointers inside the transaction.
    ///
    /// Keyset reads go through a transaction, which holds the global keyset lock,
    /// so no other keyset transaction can commit between reads. The signatory
    /// uses these transaction reads to reload its in-memory keysets, so the
    /// collision check and default amounts see peers' committed rotations rather
    /// than a stale snapshot.
    async fn get_active_keysets(&mut self) -> Result<HashMap<CurrencyUnit, Id>, Error>;

    /// Read all keyset infos inside the transaction. See
    /// [`get_active_keysets`](Self::get_active_keysets) for why keyset reads go
    /// through the transaction.
    async fn get_keyset_infos(&mut self) -> Result<Vec<MintKeySetInfo>, Error>;

    /// Read the keyset epoch inside the transaction.
    ///
    /// Transaction-scoped counterpart of [`KeysDatabase::keysets_epoch`], used to
    /// gate the reload under the global lock.
    async fn keysets_epoch(&mut self) -> Result<u64, Error>;
}

/// Mint Keys Database trait
#[async_trait]
pub trait KeysDatabase {
    /// Mint Keys Database Error
    type Err: Into<Error> + From<Error>;

    /// Begins a transaction
    ///
    /// All keyset reads and writes go through a transaction: it takes the global
    /// keyset advisory lock, so the signatory's reload sees a consistent
    /// snapshot. The autocommit reads that once existed here are gone; use the
    /// transaction's read methods instead.
    async fn begin_transaction<'a>(
        &'a self,
    ) -> Result<Box<dyn KeysDatabaseTransaction<'a, Self::Err> + Send + Sync + 'a>, Error>;

    /// An opaque epoch for the current keyset set.
    ///
    /// It must change whenever the set changes, both when a keyset is added and
    /// when the active pointer is reassigned. Callers only compare it for
    /// equality to decide whether to reload their in-memory view, so an epoch
    /// that misses reactivations would leave peers serving a stale active
    /// keyset. The storage decides how to compute it cheaply (e.g. a counter
    /// bumped inside every keyset-writing transaction); there is deliberately no
    /// default, since a count-based fallback would not move on reactivation.
    async fn keysets_epoch(&self) -> Result<u64, Self::Err>;
}

/// Mint Quote Database writer trait
#[async_trait]
pub trait QuotesTransaction: LockingTransaction {
    /// Mint Quotes Database Error
    type Err: Into<Error> + From<Error>;

    /// Add melt_request with quote_id, inputs_amount, and inputs_fee
    async fn add_melt_request(
        &mut self,
        quote_id: &QuoteId,
        inputs_amount: Amount<CurrencyUnit>,
        inputs_fee: Amount<CurrencyUnit>,
    ) -> Result<(), Self::Err>;

    /// Add blinded_messages for a quote_id
    async fn add_blinded_messages(
        &mut self,
        quote_id: Option<&QuoteId>,
        blinded_messages: &[BlindedMessage],
        operation: &Operation,
    ) -> Result<(), Self::Err>;

    /// Delete blinded_messages by their blinded secrets
    async fn delete_blinded_messages(
        &mut self,
        blinded_secrets: &[PublicKey],
    ) -> Result<(), Self::Err>;

    /// Get melt_request and associated blinded_messages by quote_id
    async fn get_melt_request_and_blinded_messages(
        &mut self,
        quote_id: &QuoteId,
    ) -> Result<Option<MeltRequestInfo>, Self::Err>;

    /// Delete melt_request and associated blinded_messages by quote_id
    async fn delete_melt_request(&mut self, quote_id: &QuoteId) -> Result<(), Self::Err>;

    /// Get [`MintMintQuote`] and lock it for update in this transaction
    async fn get_mint_quote(
        &mut self,
        quote_id: &QuoteId,
    ) -> Result<Option<Self::Locked<MintMintQuote>>, Self::Err>;

    /// Get multiple [`MintMintQuote`]s by their IDs and lock them for update in this transaction.
    ///
    /// Returns results in the same order as the input IDs, with `None` for any IDs not found.
    /// This method locks all found quotes to prevent race conditions during concurrent modifications.
    async fn get_mint_quotes_by_ids(
        &mut self,
        quote_ids: &[QuoteId],
    ) -> Result<Vec<Option<Self::Locked<MintMintQuote>>>, Self::Err>;

    /// Add [`MintMintQuote`]
    async fn add_mint_quote(
        &mut self,
        quote: MintMintQuote,
    ) -> Result<Self::Locked<MintMintQuote>, Self::Err>;

    /// Persists any pending changes made to the mint quote.
    ///
    /// This method extracts changes accumulated in the quote (via [`mint::MintQuote::take_changes`])
    /// and persists them to the database. Changes may include new payments received or new
    /// issuances recorded against the quote.
    ///
    /// If no changes are pending, this method returns successfully without performing
    /// any database operations.
    ///
    /// # Arguments
    ///
    /// * `quote` - A mutable reference to a locked mint quote. The quote
    ///   must be locked to ensure transactional consistency when persisting changes.
    ///
    /// # Implementation Notes
    ///
    /// Implementations should call [`mint::MintQuote::take_changes`] to retrieve pending
    /// changes, then persist each payment and issuance record, and finally update the
    /// quote's aggregate counters (`amount_paid`, `amount_issued`) in the database.
    async fn update_mint_quote(
        &mut self,
        quote: &mut Self::Locked<mint::MintQuote>,
    ) -> Result<(), Self::Err>;

    /// Get [`mint::MeltQuote`] and lock it for update in this transaction
    async fn get_melt_quote(
        &mut self,
        quote_id: &QuoteId,
    ) -> Result<Option<Self::Locked<mint::MeltQuote>>, Self::Err>;

    /// Add [`mint::MeltQuote`]
    async fn add_melt_quote(&mut self, quote: mint::MeltQuote) -> Result<(), Self::Err>;

    /// Retrieves all melt quotes matching a payment lookup identifier and locks them for update.
    ///
    /// This method returns multiple quotes because certain payment methods (notably BOLT12 offers)
    /// can generate multiple payment attempts that share the same lookup identifier. Locking all
    /// related quotes prevents race conditions where concurrent melt operations could interfere
    /// with each other, potentially leading to double-spending or state inconsistencies.
    ///
    /// The returned quotes are locked within the current transaction to ensure safe concurrent
    /// modification. This is essential during melt saga initiation and finalization to guarantee
    /// atomic state transitions across all related quotes.
    ///
    /// # Arguments
    ///
    /// * `request_lookup_id` - The payment identifier used by the payment backend to track
    ///   payment state (e.g., payment hash, offer ID, or label).
    async fn get_melt_quotes_by_request_lookup_id(
        &mut self,
        request_lookup_id: &PaymentIdentifier,
    ) -> Result<Vec<Self::Locked<MeltQuote>>, Self::Err>;

    /// Locks a melt quote and all related quotes sharing the same request_lookup_id atomically.
    ///
    /// This method prevents deadlocks by acquiring all locks in a single query with consistent
    /// ordering, rather than locking the target quote first and then related quotes separately.
    ///
    /// # Deadlock Prevention
    ///
    /// When multiple transactions try to melt quotes sharing the same `request_lookup_id`,
    /// acquiring locks in two steps (first the target quote, then all related quotes) can cause
    /// circular wait deadlocks. This method avoids that by:
    /// 1. Using a subquery to find the `request_lookup_id` for the target quote
    /// 2. Locking ALL quotes with that `request_lookup_id` in one atomic operation
    /// 3. Ordering locks consistently by quote ID
    ///
    /// # Arguments
    ///
    /// * `quote_id` - The ID of the target melt quote
    ///
    /// # Returns
    ///
    /// A [`LockedMeltQuotes`] containing:
    /// - `target`: The target quote (if found)
    /// - `all_related`: All quotes sharing the same `request_lookup_id` (including the target)
    ///
    /// If the quote has no `request_lookup_id`, only the target quote is returned and locked.
    async fn lock_melt_quote_and_related(
        &mut self,
        quote_id: &QuoteId,
    ) -> Result<LockedMeltQuotes<Self::Locked<MeltQuote>>, Self::Err>;

    /// Updates the request lookup id for a melt quote.
    ///
    /// Requires a locked melt quote to ensure the row is locked before modification.
    async fn update_melt_quote_request_lookup_id(
        &mut self,
        quote: &mut Self::Locked<mint::MeltQuote>,
        new_request_lookup_id: &PaymentIdentifier,
    ) -> Result<(), Self::Err>;

    /// Update [`mint::MeltQuote`] state.
    ///
    /// Requires a locked melt quote to ensure the row is locked before modification.
    /// Returns the previous state.
    async fn update_melt_quote_state(
        &mut self,
        quote: &mut Self::Locked<mint::MeltQuote>,
        new_state: MeltQuoteState,
        payment_proof: Option<String>,
    ) -> Result<MeltQuoteState, Self::Err>;

    /// Get all [`MintMintQuote`]s and lock it for update in this transaction
    async fn get_mint_quote_by_request(
        &mut self,
        request: &str,
    ) -> Result<Option<Self::Locked<MintMintQuote>>, Self::Err>;

    /// Get all [`MintMintQuote`]s
    async fn get_mint_quote_by_request_lookup_id(
        &mut self,
        request_lookup_id: &PaymentIdentifier,
    ) -> Result<Option<Self::Locked<MintMintQuote>>, Self::Err>;
}

/// Mint Quote Database trait
#[async_trait]
pub trait QuotesDatabase {
    /// Mint Quotes Database Error
    type Err: Into<Error> + From<Error>;

    /// Get [`MintMintQuote`]
    async fn get_mint_quote(&self, quote_id: &QuoteId) -> Result<Option<MintMintQuote>, Self::Err>;

    /// Atomically claim a payment backend status check for a mint quote.
    ///
    /// Returns `true` when `last_checked` was updated, or `false` when another check occurred
    /// within `min_interval` seconds.
    async fn try_update_mint_quote_last_checked(
        &self,
        quote_id: &QuoteId,
        last_checked: u64,
        min_interval: u64,
    ) -> Result<bool, Self::Err>;

    /// Get multiple [`MintMintQuote`]s by their IDs.
    ///
    /// Returns results in the same order as the input IDs, with `None` for any IDs not found.
    async fn get_mint_quotes_by_ids(
        &self,
        quote_ids: &[QuoteId],
    ) -> Result<Vec<Option<MintMintQuote>>, Self::Err>;

    /// Get all [`MintMintQuote`]s
    async fn get_mint_quote_by_request(
        &self,
        request: &str,
    ) -> Result<Option<MintMintQuote>, Self::Err>;
    /// Get all [`MintMintQuote`]s
    async fn get_mint_quote_by_request_lookup_id(
        &self,
        request_lookup_id: &PaymentIdentifier,
    ) -> Result<Option<MintMintQuote>, Self::Err>;
    /// Get Mint Quotes
    async fn get_mint_quotes(&self) -> Result<Vec<MintMintQuote>, Self::Err>;
    /// Get [`mint::MeltQuote`]
    async fn get_melt_quote(
        &self,
        quote_id: &QuoteId,
    ) -> Result<Option<mint::MeltQuote>, Self::Err>;
    /// Get all [`mint::MeltQuote`]s
    async fn get_melt_quotes(&self) -> Result<Vec<mint::MeltQuote>, Self::Err>;
}

/// Mint Proof Transaction trait
#[async_trait]
pub trait ProofsTransaction: LockingTransaction {
    /// Mint Proof Database Error
    type Err: Into<Error> + From<Error>;

    /// Add  [`Proofs`]
    ///
    /// Adds proofs to the database. The database should error if the proof already exits, with a
    /// `AttemptUpdateSpentProof` if the proof is already spent or a `Duplicate` error otherwise.
    async fn add_proofs(
        &mut self,
        proof: Proofs,
        quote_id: Option<QuoteId>,
        operation: &Operation,
    ) -> Result<Self::Locked<ProofsWithState>, Self::Err>;

    /// Updates the proofs to the given state in the database.
    ///
    /// Also updates the `state` field on the [`ProofsWithState`] wrapper to reflect
    /// the new state after the database update succeeds.
    async fn update_proofs_state(
        &mut self,
        proofs: &mut Self::Locked<ProofsWithState>,
        new_state: State,
    ) -> Result<(), Self::Err>;

    /// get proofs states
    async fn get_proofs(
        &mut self,
        ys: &[PublicKey],
    ) -> Result<Self::Locked<ProofsWithState>, Self::Err>;

    /// Remove [`Proofs`]
    async fn remove_proofs(
        &mut self,
        ys: &[PublicKey],
        quote_id: Option<QuoteId>,
    ) -> Result<(), Self::Err>;

    /// Get ys by quote id
    async fn get_proof_ys_by_quote_id(
        &mut self,
        quote_id: &QuoteId,
    ) -> Result<Vec<PublicKey>, Self::Err>;

    /// Get proof ys by operation id
    async fn get_proof_ys_by_operation_id(
        &mut self,
        operation_id: &uuid::Uuid,
    ) -> Result<Vec<PublicKey>, Self::Err>;
}

/// Mint Proof Database trait
#[async_trait]
pub trait ProofsDatabase {
    /// Mint Proof Database Error
    type Err: Into<Error> + From<Error>;

    /// Get [`Proofs`] by ys
    async fn get_proofs_by_ys(&self, ys: &[PublicKey]) -> Result<Vec<Option<Proof>>, Self::Err>;
    /// Get ys by quote id
    async fn get_proof_ys_by_quote_id(
        &self,
        quote_id: &QuoteId,
    ) -> Result<Vec<PublicKey>, Self::Err>;
    /// Get [`Proofs`] state
    async fn get_proofs_states(&self, ys: &[PublicKey]) -> Result<Vec<Option<State>>, Self::Err>;

    /// Get [`Proofs`] by state
    async fn get_proofs_by_keyset_id(
        &self,
        keyset_id: &Id,
    ) -> Result<(Proofs, Vec<Option<State>>), Self::Err>;

    /// Get total proofs redeemed by keyset id
    async fn get_total_redeemed(&self) -> Result<HashMap<Id, Amount>, Self::Err>;

    /// Get proof ys by operation id
    async fn get_proof_ys_by_operation_id(
        &self,
        operation_id: &uuid::Uuid,
    ) -> Result<Vec<PublicKey>, Self::Err>;
}

#[async_trait]
/// Mint Signatures Transaction trait
pub trait SignaturesTransaction {
    /// Mint Signature Database Error
    type Err: Into<Error> + From<Error>;

    /// Add [`BlindSignature`]
    async fn add_blind_signatures(
        &mut self,
        blinded_messages: &[PublicKey],
        blind_signatures: &[BlindSignature],
        quote_id: Option<QuoteId>,
    ) -> Result<(), Self::Err>;

    /// Get [`BlindSignature`]s
    async fn get_blind_signatures(
        &mut self,
        blinded_messages: &[PublicKey],
    ) -> Result<Vec<Option<BlindSignature>>, Self::Err>;
}

#[async_trait]
/// Mint Signatures Database trait
pub trait SignaturesDatabase {
    /// Mint Signature Database Error
    type Err: Into<Error> + From<Error>;

    /// Get [`BlindSignature`]s
    async fn get_blind_signatures(
        &self,
        blinded_messages: &[PublicKey],
    ) -> Result<Vec<Option<BlindSignature>>, Self::Err>;

    /// Get [`BlindSignature`]s for keyset_id
    async fn get_blind_signatures_for_keyset(
        &self,
        keyset_id: &Id,
    ) -> Result<Vec<BlindSignature>, Self::Err>;

    /// Get [`BlindSignature`]s for quote
    async fn get_blind_signatures_for_quote(
        &self,
        quote_id: &QuoteId,
    ) -> Result<Vec<BlindSignature>, Self::Err>;

    /// Get total amount issued by keyset id
    async fn get_total_issued(&self) -> Result<HashMap<Id, Amount>, Self::Err>;

    /// Get blinded secrets (B values) by operation id
    async fn get_blinded_secrets_by_operation_id(
        &self,
        operation_id: &uuid::Uuid,
    ) -> Result<Vec<PublicKey>, Self::Err>;
}

#[async_trait]
/// Saga Transaction trait
pub trait SagaTransaction: LockingTransaction {
    /// Saga Database Error
    type Err: Into<Error> + From<Error>;

    /// Get saga by operation_id
    async fn get_saga(
        &mut self,
        operation_id: &uuid::Uuid,
    ) -> Result<Option<mint::Saga>, Self::Err>;

    /// Get a saga by operation ID and lock it for mutation.
    ///
    /// Implementations must lock the returned row for the lifetime of the
    /// transaction. Saga mutation methods require this marker so callers
    /// cannot update a stale snapshot loaded outside the transaction.
    async fn get_saga_for_update(
        &mut self,
        operation_id: &uuid::Uuid,
    ) -> Result<Option<Self::Locked<mint::Saga>>, Self::Err>;

    /// Add saga
    async fn add_saga(&mut self, saga: &mint::Saga) -> Result<(), Self::Err>;

    /// Update the state of a locked saga.
    async fn update_acquired_saga(
        &mut self,
        saga: &mut Self::Locked<mint::Saga>,
        new_state: mint::SagaStateEnum,
    ) -> Result<(), Self::Err>;

    /// Update a locked saga state and optional finalization metadata.
    async fn update_acquired_saga_with_finalization_data(
        &mut self,
        saga: &mut Self::Locked<mint::Saga>,
        new_state: mint::SagaStateEnum,
        finalization_data: Option<&mint::MeltFinalizationData>,
    ) -> Result<(), Self::Err>;

    /// Delete saga
    async fn delete_saga(&mut self, operation_id: &uuid::Uuid) -> Result<(), Self::Err>;
}

#[async_trait]
/// Saga Database trait
pub trait SagaDatabase {
    /// Saga Database Error
    type Err: Into<Error> + From<Error>;

    /// Get the melt saga associated with a melt quote id
    async fn get_melt_saga_by_quote_id(
        &self,
        quote_id: &QuoteId,
    ) -> Result<Option<mint::Saga>, Self::Err>;

    /// Get all incomplete sagas for a given operation kind
    async fn get_incomplete_sagas(
        &self,
        operation_kind: mint::OperationKind,
    ) -> Result<Vec<mint::Saga>, Self::Err>;
}

#[async_trait]
/// Completed Operations Transaction trait
pub trait CompletedOperationsTransaction {
    /// Completed Operations Database Error
    type Err: Into<Error> + From<Error>;

    /// Add completed operation
    async fn add_completed_operation(
        &mut self,
        operation: &mint::Operation,
        fee_by_keyset: &std::collections::HashMap<crate::nuts::Id, crate::Amount>,
    ) -> Result<(), Self::Err>;
}

#[async_trait]
/// Completed Operations Database trait
pub trait CompletedOperationsDatabase {
    /// Completed Operations Database Error
    type Err: Into<Error> + From<Error>;

    /// Get completed operation by operation_id
    async fn get_completed_operation(
        &self,
        operation_id: &uuid::Uuid,
    ) -> Result<Option<mint::Operation>, Self::Err>;

    /// Get completed operations by operation kind
    async fn get_completed_operations_by_kind(
        &self,
        operation_kind: mint::OperationKind,
    ) -> Result<Vec<mint::Operation>, Self::Err>;

    /// Get all completed operations
    async fn get_completed_operations(&self) -> Result<Vec<mint::Operation>, Self::Err>;
}

/// Base database writer
///
/// Quote advisory locks coordinate transactional quote updates. Callers must
/// acquire all quote locks before taking row locks, and implementations must
/// acquire a batch in stable order. Locks are held until the transaction
/// commits or rolls back.
#[async_trait]
pub trait Transaction<Error>:
    DbTransactionFinalizer<Err = Error>
    + QuotesTransaction<Err = Error>
    + SignaturesTransaction<Err = Error>
    + ProofsTransaction<Err = Error>
    + KVStoreTransaction<Error>
    + SagaTransaction<Err = Error>
    + CompletedOperationsTransaction<Err = Error>
{
    /// Lock a set of quote identifiers for this transaction.
    ///
    /// Returns `true` when the backend acquired a cross-process lock. Backends
    /// without quote advisory locks return `false`.
    async fn lock_quotes(&mut self, _quote_ids: &[QuoteId]) -> Result<bool, Error> {
        Ok(false)
    }
}

/// Mint Database trait
#[async_trait]
pub trait Database<Error>:
    KVStoreDatabase<Err = Error>
    + QuotesDatabase<Err = Error>
    + ProofsDatabase<Err = Error>
    + SignaturesDatabase<Err = Error>
    + SagaDatabase<Err = Error>
    + CompletedOperationsDatabase<Err = Error>
{
    /// The transaction this database begins
    type Transaction: Transaction<Error> + Send + Sync + 'static;

    /// Begins a transaction
    async fn begin_transaction(&self) -> Result<Box<Self::Transaction>, Error>;
}

/// A mint database that can be shared between tasks.
///
/// Shorthand for the bounds the mint puts on its storage. It is implemented for
/// every type that satisfies them.
pub trait MintStore: Database<Error> + Send + Sync + 'static {}

impl<D> MintStore for D where D: Database<Error> + Send + Sync + 'static {}

/// A `T` locked by a transaction of the mint database `D`
pub type LockedBy<D, T> = <<D as Database<Error>>::Transaction as LockingTransaction>::Locked<T>;
