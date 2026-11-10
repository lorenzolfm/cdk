-- Replace the per-keyset counter row with an append-only ledger. Each write
-- appends a row, so concurrent operations on the same keyset never update a
-- shared row.
CREATE TABLE keyset_ledger (
    id BIGSERIAL PRIMARY KEY,
    keyset_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('issued', 'redeemed', 'fee')),
    amount BIGINT NOT NULL CHECK (amount > 0)
);

CREATE INDEX idx_keyset_ledger_kind_keyset ON keyset_ledger(kind, keyset_id);

-- Carry the existing totals over as one opening entry per keyset and kind.
INSERT INTO keyset_ledger (keyset_id, kind, amount)
SELECT keyset_id, 'issued', total_issued FROM keyset_amounts WHERE total_issued > 0;

INSERT INTO keyset_ledger (keyset_id, kind, amount)
SELECT keyset_id, 'redeemed', total_redeemed FROM keyset_amounts WHERE total_redeemed > 0;

INSERT INTO keyset_ledger (keyset_id, kind, amount)
SELECT keyset_id, 'fee', fee_collected FROM keyset_amounts WHERE fee_collected > 0;

DROP TABLE keyset_amounts;
