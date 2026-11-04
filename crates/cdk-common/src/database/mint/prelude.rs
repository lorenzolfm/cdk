//! Brings the mint database and transaction traits into scope, without their names.
//!
//! A method on a generic transaction such as `D::Transaction`, or on a concrete
//! backend, only resolves when its trait is in scope, including the supertraits
//! of [`Transaction`](super::Transaction) and [`Database`](super::Database).

pub use super::super::DbTransactionFinalizer as _;
pub use super::{
    CompletedOperationsDatabase as _, CompletedOperationsTransaction as _, Database as _,
    KVStoreDatabase as _, KVStoreTransaction as _, LockedValue as _, ProofsDatabase as _,
    ProofsTransaction as _, QuotesDatabase as _, QuotesTransaction as _, SagaDatabase as _,
    SagaTransaction as _, SignaturesDatabase as _, SignaturesTransaction as _, Transaction as _,
};
