//! The row wrapper this backend hands out for locked values

use std::ops::{Deref, DerefMut};

use cdk_common::database::LockedValue;

/// A wrapper indicating that a value has been acquired with a database lock.
///
/// This type is returned by database operations that lock rows for update
/// (e.g., `SELECT ... FOR UPDATE`) or inserted them. Only this crate can build
/// one, so holding an `Acquired<T>` means this crate's queries returned the value.
///
/// # Deref Behavior
///
/// `Acquired<T>` implements `Deref` and `DerefMut`, allowing transparent access
/// to the inner value's methods and fields.
///
/// # Construction
///
/// A row a locking query returned can be written:
///
/// ```
/// use cdk_common::database::mint::prelude::*;
/// use cdk_common::database::Error;
/// use cdk_common::QuoteId;
/// use cdk_sql_common::mint::SQLTransaction;
/// use cdk_sql_common::pool::DatabasePool;
///
/// async fn write_locked<RM>(tx: &mut SQLTransaction<RM>, id: &QuoteId) -> Result<(), Error>
/// where
///     RM: DatabasePool + 'static,
/// {
///     if let Some(mut quote) = tx.get_mint_quote(id).await? {
///         tx.update_mint_quote(&mut quote).await?;
///     }
///     Ok(())
/// }
/// ```
///
/// Outside this crate, a plain value can't be turned into one with the
/// constructor:
///
/// ```compile_fail,E0624
/// use cdk_common::mint::MintQuote;
/// use cdk_sql_common::mint::Acquired;
///
/// fn wrap(quote: MintQuote) -> Acquired<MintQuote> {
///     Acquired::new(quote)
/// }
/// ```
///
/// with a conversion:
///
/// ```compile_fail,E0277
/// use cdk_common::mint::MintQuote;
/// use cdk_sql_common::mint::Acquired;
///
/// fn wrap(quote: MintQuote) -> Acquired<MintQuote> {
///     quote.into()
/// }
/// ```
///
/// or with a struct literal:
///
/// ```compile_fail,E0451
/// use cdk_common::mint::MintQuote;
/// use cdk_sql_common::mint::Acquired;
///
/// fn wrap(inner: MintQuote) -> Acquired<MintQuote> {
///     Acquired { inner }
/// }
/// ```
///
/// Nor can a transaction write a wrapper defined somewhere else:
///
/// ```compile_fail,E0308
/// use std::ops::{Deref, DerefMut};
///
/// use cdk_common::database::mint::prelude::*;
/// use cdk_common::database::Error;
/// use cdk_common::mint::MintQuote;
/// use cdk_sql_common::mint::SQLTransaction;
/// use cdk_sql_common::pool::DatabasePool;
///
/// struct Forged(MintQuote);
///
/// impl Deref for Forged {
///     type Target = MintQuote;
///     fn deref(&self) -> &MintQuote {
///         &self.0
///     }
/// }
///
/// impl DerefMut for Forged {
///     fn deref_mut(&mut self) -> &mut MintQuote {
///         &mut self.0
///     }
/// }
///
/// async fn write_forged<RM>(tx: &mut SQLTransaction<RM>, mut quote: Forged) -> Result<(), Error>
/// where
///     RM: DatabasePool + 'static,
/// {
///     tx.update_mint_quote(&mut quote).await
/// }
/// ```
#[derive(Debug)]
pub struct Acquired<T> {
    inner: T,
}

impl<T> Acquired<T> {
    /// Wraps a value this crate just locked.
    pub(crate) fn new(inner: T) -> Self {
        Self { inner }
    }
}

impl<T> LockedValue<T> for Acquired<T>
where
    T: Send,
{
    fn into_inner(self) -> T {
        self.inner
    }
}

impl<T> Deref for Acquired<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T> DerefMut for Acquired<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
