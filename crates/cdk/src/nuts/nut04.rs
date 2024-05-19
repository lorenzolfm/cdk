//! NUT-04: Mint Tokens via Bolt11
//!
//! <https://github.com/cashubtc/nuts/blob/main/04.md>

use serde::{Deserialize, Serialize};

use super::nut00::{BlindSignature, BlindedMessage, CurrencyUnit, PaymentMethod};
use crate::types::MintQuote;
use crate::Amount;

/// Mint quote request [NUT-04]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintQuoteBolt11Request {
    /// Amount
    pub amount: Amount,
    /// Unit wallet would like to pay with
    pub unit: CurrencyUnit,
}

/// Mint quote response [NUT-04]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintQuoteBolt11Response {
    /// Quote Id
    pub quote: String,
    /// Payment request to fulfil
    pub request: String,
    /// Whether the the request haas be paid
    pub paid: bool,
    /// Unix timestamp until the quote is valid
    pub expiry: u64,
}

impl From<MintQuote> for MintQuoteBolt11Response {
    fn from(mint_quote: MintQuote) -> MintQuoteBolt11Response {
        MintQuoteBolt11Response {
            quote: mint_quote.id,
            request: mint_quote.request,
            paid: mint_quote.paid,
            expiry: mint_quote.expiry,
        }
    }
}

/// Mint request [NUT-04]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintBolt11Request {
    /// Quote id
    pub quote: String,
    /// Outputs
    pub outputs: Vec<BlindedMessage>,
}

impl MintBolt11Request {
    pub fn total_amount(&self) -> Amount {
        self.outputs
            .iter()
            .map(|BlindedMessage { amount, .. }| *amount)
            .sum()
    }
}

/// Mint response [NUT-04]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintBolt11Response {
    /// Blinded Signatures
    pub signatures: Vec<BlindSignature>,
}

/// Mint Method Settings
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MintMethodSettings {
    /// Payment Method e.g. bolt11
    pub method: PaymentMethod,
    /// Currency Unit e.g. sat
    pub unit: CurrencyUnit,
    /// Min Amount
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_amount: Option<Amount>,
    /// Max Amount
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_amount: Option<Amount>,
}

/// Mint Settings
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Settings {
    pub methods: Vec<MintMethodSettings>,
    pub disabled: bool,
}
