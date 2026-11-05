# CDK Acquired

Marks a row as locked by the current mint database transaction.

Only mint storage crates should depend on this crate. `cdk-common` re-exports the
`Acquired` type but not `from_locked`, so business logic can name the type and can't
build one.
