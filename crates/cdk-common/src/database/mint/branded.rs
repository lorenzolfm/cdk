//! Locked rows tied to their transaction at compile time.
//!
//! [`Acquired`] marks a row as locked, but nothing connects it to the transaction
//! that locked it. [`BrandedTransaction`] and [`BrandedAcquired`] share a lifetime
//! that is unique to one transaction, so the compiler rejects a row locked by a
//! different transaction.
//!
//! This wraps [`DynMintTransaction`], so database backends don't change. It only
//! forwards the methods that the mint's payment notification path uses.
//!
//! # Usage
//!
//! Create a guard in the scope that holds the transaction. Helpers name `'id` once
//! for both the transaction and the row. A brand can't leave the scope that created
//! its guard, so a function that begins and returns a transaction takes the guard
//! as an argument.
//!
//! ```no_run
//! use cdk_common::database::mint::branded::{
//!     make_guard, BrandedAcquired, BrandedTransaction, Guard,
//! };
//! use cdk_common::database::{DynMintDatabase, Error};
//! use cdk_common::mint::MintQuote;
//! use cdk_common::payment::PaymentIdentifier;
//!
//! async fn persist<'id>(
//!     tx: &mut BrandedTransaction<'id>,
//!     quote: &mut BrandedAcquired<'id, MintQuote>,
//! ) -> Result<(), Error> {
//!     tx.update_mint_quote(quote).await
//! }
//!
//! async fn begin_and_lock<'id>(
//!     db: &DynMintDatabase,
//!     guard: Guard<'id>,
//!     lookup_id: &PaymentIdentifier,
//! ) -> Result<(BrandedTransaction<'id>, BrandedAcquired<'id, MintQuote>), Error> {
//!     let mut tx = BrandedTransaction::begin(db, guard).await?;
//!     let quote = tx
//!         .get_mint_quote_by_request_lookup_id(lookup_id)
//!         .await?
//!         .ok_or(Error::UnknownQuote)?;
//!     Ok((tx, quote))
//! }
//!
//! async fn example(
//!     db: DynMintDatabase,
//!     lookup_a: PaymentIdentifier,
//!     lookup_b: PaymentIdentifier,
//! ) -> Result<(), Error> {
//!     make_guard!(guard_a);
//!     let (mut tx_a, mut quote_a) = begin_and_lock(&db, guard_a, &lookup_a).await?;
//!     make_guard!(guard_b);
//!     let (mut tx_b, mut quote_b) = begin_and_lock(&db, guard_b, &lookup_b).await?;
//!
//!     persist(&mut tx_a, &mut quote_a).await?;
//!     persist(&mut tx_b, &mut quote_b).await?;
//!
//!     tx_a.commit().await?;
//!     tx_b.commit().await?;
//!
//!     // The value stays readable after commit, but no transaction accepts it.
//!     let _paid = quote_a.amount_paid();
//!     Ok(())
//! }
//! ```
//!
//! # What the compiler rejects
//!
//! A row locked by another transaction:
//!
//! ```compile_fail
//! # use cdk_common::database::mint::branded::{make_guard, BrandedTransaction};
//! # use cdk_common::database::{DynMintDatabase, Error};
//! # use cdk_common::payment::PaymentIdentifier;
//! # async fn example(db: DynMintDatabase, lookup_id: PaymentIdentifier) -> Result<(), Error> {
//! make_guard!(guard_a);
//! let mut tx_a = BrandedTransaction::begin(&db, guard_a).await?;
//! make_guard!(guard_b);
//! let mut tx_b = BrandedTransaction::begin(&db, guard_b).await?;
//!
//! let mut quote = tx_a
//!     .get_mint_quote_by_request_lookup_id(&lookup_id)
//!     .await?
//!     .ok_or(Error::UnknownQuote)?;
//! tx_b.update_mint_quote(&mut quote).await?;
//! # Ok(())
//! # }
//! ```
//!
//! A value read outside the transaction:
//!
//! ```compile_fail
//! # use cdk_common::database::mint::branded::{make_guard, BrandedAcquired, BrandedTransaction};
//! # use cdk_common::database::{DynMintDatabase, Error};
//! # use cdk_common::mint::MintQuote;
//! # use cdk_common::QuoteId;
//! # async fn example(db: DynMintDatabase, quote_id: QuoteId) -> Result<(), Error> {
//! make_guard!(guard);
//! let mut tx = BrandedTransaction::begin(&db, guard).await?;
//!
//! let quote = db.get_mint_quote(&quote_id).await?.ok_or(Error::UnknownQuote)?;
//! let mut quote: BrandedAcquired<'_, MintQuote> = quote.into();
//! tx.update_mint_quote(&mut quote).await?;
//! # Ok(())
//! # }
//! ```
//!
//! An [`Acquired`] built by hand:
//!
//! ```compile_fail
//! # use cdk_common::database::mint::branded::{make_guard, BrandedTransaction};
//! # use cdk_common::database::mint::Acquired;
//! # use cdk_common::database::{DynMintDatabase, Error};
//! # use cdk_common::mint::MintQuote;
//! # use cdk_common::QuoteId;
//! # async fn example(db: DynMintDatabase, quote_id: QuoteId) -> Result<(), Error> {
//! make_guard!(guard);
//! let mut tx = BrandedTransaction::begin(&db, guard).await?;
//!
//! let quote = db.get_mint_quote(&quote_id).await?.ok_or(Error::UnknownQuote)?;
//! let mut quote: Acquired<MintQuote> = quote.into();
//! tx.update_mint_quote(&mut quote).await?;
//! # Ok(())
//! # }
//! ```

use core::fmt;
use std::ops::{Deref, DerefMut};

use generativity::Id;
pub use generativity::{make_guard, Guard};

use super::{Acquired, DynMintDatabase, DynMintTransaction};
use crate::database::Error;
use crate::mint::MintQuote;
use crate::payment::PaymentIdentifier;

/// A row locked by the [`BrandedTransaction`] with the same `'id`.
///
/// Only [`BrandedTransaction`] builds this type. It stays readable after the
/// transaction ends, but no other transaction accepts it.
#[derive(Debug)]
pub struct BrandedAcquired<'id, T> {
    inner: Acquired<T>,
    _transaction: Id<'id>,
}

impl<T> Deref for BrandedAcquired<'_, T> {
    type Target = T;

    /// Returns a reference to the inner resource.
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T> DerefMut for BrandedAcquired<'_, T> {
    /// Returns a mutable reference to the inner resource.
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

/// A mint transaction with a compile-time identity.
///
/// Every [`BrandedAcquired`] it returns carries its `'id`, and its update methods
/// only accept a [`BrandedAcquired`] with that same `'id`.
pub struct BrandedTransaction<'id> {
    inner: DynMintTransaction,
    id: Id<'id>,
}

impl fmt::Debug for BrandedTransaction<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrandedTransaction").finish_non_exhaustive()
    }
}

impl<'id> BrandedTransaction<'id> {
    /// Begins a transaction, consuming the guard that gives it a unique `'id`.
    pub async fn begin(db: &DynMintDatabase, guard: Guard<'id>) -> Result<Self, Error> {
        let id = guard.into();

        Ok(Self {
            inner: db.begin_transaction().await?,
            id,
        })
    }

    /// Get the [`MintQuote`] for a payment lookup identifier and lock it for update.
    pub async fn get_mint_quote_by_request_lookup_id(
        &mut self,
        request_lookup_id: &PaymentIdentifier,
    ) -> Result<Option<BrandedAcquired<'id, MintQuote>>, Error> {
        let quote = self
            .inner
            .get_mint_quote_by_request_lookup_id(request_lookup_id)
            .await?;

        Ok(quote.map(|inner| BrandedAcquired {
            inner,
            _transaction: self.id,
        }))
    }

    /// Persists any pending changes made to a mint quote locked by this transaction.
    pub async fn update_mint_quote(
        &mut self,
        quote: &mut BrandedAcquired<'id, MintQuote>,
    ) -> Result<(), Error> {
        self.inner.update_mint_quote(&mut quote.inner).await
    }

    /// Commits all the changes into the database.
    pub async fn commit(self) -> Result<(), Error> {
        self.inner.commit().await
    }
}
