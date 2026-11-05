//! Checks that this crate can't build an `Acquired`.
//!
//! `Acquired` marks a row as locked by the current transaction. It belongs to
//! `cdk-sql-common` and its constructor is private to that crate. These doctests
//! fail if that changes.
//!
//! The type stays usable from here:
//!
//! ```
//! use cdk::cdk_database::Acquired;
//! use cdk_common::mint::MintQuote;
//!
//! fn quote_id(quote: &Acquired<MintQuote>) -> String {
//!     quote.id.to_string()
//! }
//! ```
//!
//! A plain row doesn't convert:
//!
//! ```compile_fail,E0277
//! use cdk::cdk_database::Acquired;
//! use cdk_common::mint::MintQuote;
//!
//! fn forge(quote: MintQuote) -> Acquired<MintQuote> {
//!     quote.into()
//! }
//! ```
//!
//! The field is private:
//!
//! ```compile_fail,E0451
//! use cdk::cdk_database::Acquired;
//! use cdk_common::mint::MintQuote;
//!
//! fn forge(quote: MintQuote) -> Acquired<MintQuote> {
//!     Acquired { inner: quote }
//! }
//! ```
//!
//! The constructor is private to `cdk-sql-common`:
//!
//! ```compile_fail,E0624
//! use cdk::cdk_database::Acquired;
//! use cdk_common::mint::MintQuote;
//!
//! fn forge(quote: MintQuote) -> Acquired<MintQuote> {
//!     Acquired::new(quote)
//! }
//! ```
//!
//! This is what pinning costs: a backend with its own locked rows doesn't fit.
//!
//! ```compile_fail,E0271
//! use std::ops::{Deref, DerefMut};
//! use std::sync::Arc;
//!
//! use cdk::cdk_database::{self, DynMintDatabase, MintDatabase};
//! use cdk_common::mint::{MeltQuote, MintQuote, ProofsWithState, Saga};
//!
//! #[derive(Debug)]
//! struct OwnLocked<T>(T);
//!
//! impl<T> Deref for OwnLocked<T> {
//!     type Target = T;
//!     fn deref(&self) -> &T {
//!         &self.0
//!     }
//! }
//!
//! impl<T> DerefMut for OwnLocked<T> {
//!     fn deref_mut(&mut self) -> &mut T {
//!         &mut self.0
//!     }
//! }
//!
//! fn into_mint_database<D>(db: D) -> DynMintDatabase
//! where
//!     D: MintDatabase<
//!             cdk_database::Error,
//!             LockedMintQuote = OwnLocked<MintQuote>,
//!             LockedMeltQuote = OwnLocked<MeltQuote>,
//!             LockedProofs = OwnLocked<ProofsWithState>,
//!             LockedSaga = OwnLocked<Saga>,
//!         > + Send
//!         + Sync
//!         + 'static,
//! {
//!     Arc::new(db)
//! }
//! ```
