//! Locked rows, owned by this crate.

use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use cdk_common::database::{Error, MintDatabase, MintTransaction};
use cdk_common::mint::{MeltQuote, MintQuote, ProofsWithState, Saga};

/// A wrapper indicating that a resource has been acquired with a database lock.
///
/// This type is returned by database operations that lock rows for update
/// (e.g., `SELECT ... FOR UPDATE`). It serves as a compile-time marker that
/// the wrapped resource was properly locked before being returned, ensuring
/// that subsequent modifications are safe from race conditions.
///
/// Only this crate can build one, because the mint traits name it through their
/// associated types and its constructor is private to this crate.
///
/// # Usage
///
/// When you need to modify a database record, first acquire it using a locking
/// query method. The returned `Acquired<T>` guarantees the row is locked for
/// the duration of the transaction.
///
/// ```ignore
/// // Acquire a quote with a row lock
/// let mut quote: Acquired<MintQuote> = tx.get_mint_quote(&quote_id).await?;
///
/// // Safely modify the quote (row is locked)
/// quote.state = QuoteState::Paid;
///
/// // Persist the changes
/// tx.update_mint_quote(&mut quote).await?;
/// ```
///
/// # Deref Behavior
///
/// `Acquired<T>` implements `Deref` and `DerefMut`, allowing transparent access
/// to the inner value's methods and fields.
#[derive(Debug)]
pub struct Acquired<T> {
    inner: T,
}

impl<T> Acquired<T> {
    /// Marks a value as locked by the current database transaction.
    ///
    /// Call this right after running the locking query or the insert that
    /// produced the value.
    pub(crate) fn new(value: T) -> Self {
        Self { inner: value }
    }

    /// Consumes the wrapper and returns the inner resource.
    ///
    /// Use this when you need to take ownership of the inner value,
    /// for example when passing it to a function that doesn't accept
    /// `Acquired<T>`.
    pub fn inner(self) -> T {
        self.inner
    }
}

impl<T> Deref for Acquired<T> {
    type Target = T;

    /// Returns a reference to the inner resource.
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T> DerefMut for Acquired<T> {
    /// Returns a mutable reference to the inner resource.
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// Type alias for Mint Database, pinned to this crate's locked rows
pub type DynMintDatabase = Arc<
    dyn MintDatabase<
            Error,
            LockedMintQuote = Acquired<MintQuote>,
            LockedMeltQuote = Acquired<MeltQuote>,
            LockedProofs = Acquired<ProofsWithState>,
            LockedSaga = Acquired<Saga>,
        > + Send
        + Sync,
>;

/// Type alias for Mint Transaction, pinned to this crate's locked rows
pub type DynMintTransaction = Box<
    dyn MintTransaction<
            Error,
            LockedMintQuote = Acquired<MintQuote>,
            LockedMeltQuote = Acquired<MeltQuote>,
            LockedProofs = Acquired<ProofsWithState>,
            LockedSaga = Acquired<Saga>,
        > + Send
        + Sync,
>;
