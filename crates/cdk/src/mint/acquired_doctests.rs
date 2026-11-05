//! Checks that this crate can't build an `Acquired`.
//!
//! `Acquired` marks a row as locked by the current transaction. `cdk` names the
//! type through `cdk-common`, but it doesn't depend on `cdk-acquired`, so it can't
//! reach the constructor. These doctests fail if that changes.
//!
//! The type stays usable from here:
//!
//! ```
//! use cdk_common::database::mint::Acquired;
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
//! use cdk_common::database::mint::Acquired;
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
//! use cdk_common::database::mint::Acquired;
//! use cdk_common::mint::MintQuote;
//!
//! fn forge(quote: MintQuote) -> Acquired<MintQuote> {
//!     Acquired { inner: quote }
//! }
//! ```
//!
//! There's no associated constructor to reach through the re-export:
//!
//! ```compile_fail,E0599
//! use cdk_common::database::mint::Acquired;
//! use cdk_common::mint::MintQuote;
//!
//! fn forge(quote: MintQuote) -> Acquired<MintQuote> {
//!     Acquired::from_locked(quote)
//! }
//! ```
//!
//! And the crate that has the constructor isn't a dependency:
//!
//! ```compile_fail,E0433
//! use cdk_common::database::mint::Acquired;
//! use cdk_common::mint::MintQuote;
//!
//! fn forge(quote: MintQuote) -> Acquired<MintQuote> {
//!     cdk_acquired::from_locked(quote)
//! }
//! ```
