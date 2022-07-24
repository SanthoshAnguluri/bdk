//! Esplora
//!
//! This module defines a [`EsploraBlockchain`] struct that can query an Esplora
//! backend populate the wallet's [database](crate::database::Database) by:
//!
//! ## Example
//!
//! ```no_run
//! # use bdk::blockchain::esplora::EsploraBlockchain;
//! let blockchain = EsploraBlockchain::new("https://blockstream.info/testnet/api", 20);
//! # Ok::<(), bdk::Error>(())
//! ```
//!
//! Esplora blockchain can use either `ureq` or `reqwest` for the HTTP client
//! depending on your needs (blocking or async respectively).
//!
//! Please note, to configure the Esplora HTTP client correctly use one of:
//! Blocking:  --features='esplora,ureq'
//! Async:     --features='async-interface,esplora,reqwest' --no-default-features
use std::collections::HashMap;
use std::fmt;
use std::io;

use bitcoin::consensus;
use bitcoin::{BlockHash, Txid};

use crate::error::Error;
use crate::FeeRate;

#[cfg(feature = "reqwest")]
mod reqwest;

#[cfg(feature = "reqwest")]
pub use self::reqwest::*;

#[cfg(feature = "ureq")]
mod ureq;

#[cfg(feature = "ureq")]
pub use self::ureq::*;

mod api;

fn into_fee_rate(target: usize, estimates: HashMap<String, f64>) -> Result<FeeRate, Error> {
    let fee_val = {
        let mut pairs = estimates
            .into_iter()
            .filter_map(|(k, v)| Some((k.parse::<usize>().ok()?, v)))
            .collect::<Vec<_>>();
        pairs.sort_unstable_by_key(|(k, _)| std::cmp::Reverse(*k));
        pairs
            .into_iter()
            .find(|(k, _)| k <= &target)
            .map(|(_, v)| v)
            .unwrap_or(1.0)
    };
    Ok(FeeRate::from_sat_per_vb(fee_val as f32))
}

/// Errors that can happen during a sync with [`EsploraBlockchain`]
#[derive(Debug)]
pub enum EsploraError {
    /// Error during ureq HTTP request
    #[cfg(feature = "ureq")]
    Ureq(::ureq::Error),
    /// Transport error during the ureq HTTP call
    #[cfg(feature = "ureq")]
    UreqTransport(::ureq::Transport),
    /// Error during reqwest HTTP request
    #[cfg(feature = "reqwest")]
    Reqwest(::reqwest::Error),
    /// HTTP response error
    HttpResponse(u16),
    /// IO error during ureq response read
    Io(io::Error),
    /// No header found in ureq response
    NoHeader,
    /// Invalid number returned
    Parsing(std::num::ParseIntError),
    /// Invalid Bitcoin data returned
    BitcoinEncoding(bitcoin::consensus::encode::Error),
    /// Invalid Hex data returned
    Hex(bitcoin::hashes::hex::Error),

    /// Transaction not found
    TransactionNotFound(Txid),
    /// Header height not found
    HeaderHeightNotFound(u32),
    /// Header hash not found
    HeaderHashNotFound(BlockHash),
}

impl fmt::Display for EsploraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self)
    }
}

/// Configuration for an [`EsploraBlockchain`]
#[derive(Debug, serde::Deserialize, serde::Serialize, Clone, PartialEq)]
pub struct EsploraBlockchainConfig {
    /// Base URL of the esplora service
    ///
    /// eg. `https://blockstream.info/api/`
    pub base_url: String,
    /// Optional URL of the proxy to use to make requests to the Esplora server
    ///
    /// The string should be formatted as: `<protocol>://<user>:<password>@host:<port>`.
    ///
    /// Note that the format of this value and the supported protocols change slightly between the
    /// sync version of esplora (using `ureq`) and the async version (using `reqwest`). For more
    /// details check with the documentation of the two crates. Both of them are compiled with
    /// the `socks` feature enabled.
    ///
    /// The proxy is ignored when targeting `wasm32`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
    /// Number of parallel requests sent to the esplora service (default: 4)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<u8>,
    /// Stop searching addresses for transactions after finding an unused gap of this length.
    pub stop_gap: usize,
    /// Socket timeout.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

impl EsploraBlockchainConfig {
    /// create a config with default values given the base url and stop gap
    pub fn new(base_url: String, stop_gap: usize) -> Self {
        Self {
            base_url,
            proxy: None,
            timeout: None,
            stop_gap,
            concurrency: None,
        }
    }
}

impl std::error::Error for EsploraError {}

#[cfg(feature = "ureq")]
impl_error!(::ureq::Transport, UreqTransport, EsploraError);
#[cfg(feature = "reqwest")]
impl_error!(::reqwest::Error, Reqwest, EsploraError);
impl_error!(io::Error, Io, EsploraError);
impl_error!(std::num::ParseIntError, Parsing, EsploraError);
impl_error!(consensus::encode::Error, BitcoinEncoding, EsploraError);
impl_error!(bitcoin::hashes::hex::Error, Hex, EsploraError);

#[cfg(test)]
#[cfg(feature = "test-esplora")]
crate::bdk_blockchain_tests! {
    fn test_instance(test_client: &TestClient) -> EsploraBlockchain {
        EsploraBlockchain::new(&format!("http://{}",test_client.electrsd.esplora_url.as_ref().unwrap()), 20)
    }
}

const DEFAULT_CONCURRENT_REQUESTS: u8 = 4;

#[cfg(test)]
pub mod test {
    use super::*;
    use crate::make_blockchain_tests;
    use crate::testutils::blockchain_tests::BlockchainType;
    use crate::testutils::blockchain_tests::TestClient;

    #[cfg(feature = "test-esplora")]
    make_blockchain_tests![
        @type BlockchainType::EsploraBlockchain,
        @tests (
            test_sync_simple,
            test_sync_stop_gap_20,
            test_sync_before_and_after_receive,
            test_sync_multiple_outputs_same_tx,
            test_sync_receive_multi,
            test_sync_address_reuse,
            test_sync_receive_rbf_replaced,
            test_sync_reorg_block,
            test_sync_after_send,
            test_sync_double_receive,
            test_sync_many_sends_to_a_single_address,
            test_update_confirmation_time_after_generate,
            test_sync_outgoing_from_scratch,
            test_sync_long_change_chain,
            test_sync_bump_fee_basic,
            test_sync_bump_fee_remove_change,
            test_sync_bump_fee_add_input_simple,
            test_sync_bump_fee_add_input_no_change,
            test_add_data,
            test_sync_receive_coinbase,
            test_send_to_bech32m_addr,
            test_tx_chain,
            test_double_spend,
            test_send_receive_pkh,
            test_taproot_key_spend,
            test_taproot_script_spend,
            test_sign_taproot_core_keyspend_psbt,
            test_sign_taproot_core_scriptspend2_psbt,
            test_sign_taproot_core_scriptspend3_psbt,
        )
    ];

    #[test]
    fn feerate_parsing() {
        let esplora_fees = serde_json::from_str::<HashMap<String, f64>>(
            r#"{
  "25": 1.015,
  "5": 2.3280000000000003,
  "12": 2.0109999999999997,
  "15": 1.018,
  "17": 1.018,
  "11": 2.0109999999999997,
  "3": 3.01,
  "2": 4.9830000000000005,
  "6": 2.2359999999999998,
  "21": 1.018,
  "13": 1.081,
  "7": 2.2359999999999998,
  "8": 2.2359999999999998,
  "16": 1.018,
  "20": 1.018,
  "22": 1.017,
  "23": 1.017,
  "504": 1,
  "9": 2.2359999999999998,
  "14": 1.018,
  "10": 2.0109999999999997,
  "24": 1.017,
  "1008": 1,
  "1": 4.9830000000000005,
  "4": 2.3280000000000003,
  "19": 1.018,
  "144": 1,
  "18": 1.018
}
"#,
        )
        .unwrap();
        assert_eq!(
            into_fee_rate(6, esplora_fees.clone()).unwrap(),
            FeeRate::from_sat_per_vb(2.236)
        );
        assert_eq!(
            into_fee_rate(26, esplora_fees).unwrap(),
            FeeRate::from_sat_per_vb(1.015),
            "should inherit from value for 25"
        );
    }
}
