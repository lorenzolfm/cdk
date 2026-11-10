-- Melt finalize and startup recovery look up a quote's input proofs by
-- quote_id alone, and no index covered that column, so each lookup scanned the
-- whole proof table inside a write transaction. Only melt inputs carry a
-- quote_id, so a partial index keeps it small.
CREATE INDEX IF NOT EXISTS idx_proof_quote_id ON proof(quote_id) WHERE quote_id IS NOT NULL;
