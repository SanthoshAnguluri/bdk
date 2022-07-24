// Bitcoin Dev Kit
// Written in 2020 by Alekos Filini <alekos.filini@gmail.com>
//
// Copyright (c) 2020-2021 Bitcoin Dev Kit Developers
//
// This file is licensed under the Apache License, Version 2.0 <LICENSE-APACHE
// or http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your option.
// You may not use this file except in accordance with one or both of these
// licenses.

//! Electrum
//!
//! This module defines a [`Blockchain`] struct that wraps an [`electrum_client::Client`]
//! and implements the logic required to populate the wallet's [database](crate::database::Database) by
//! querying the inner client.
//!
//! ## Example
//!
//! ```no_run
//! # use bdk::blockchain::electrum::ElectrumBlockchain;
//! let client = electrum_client::Client::new("ssl://electrum.blockstream.info:50002")?;
//! let blockchain = ElectrumBlockchain::from(client);
//! # Ok::<(), bdk::Error>(())
//! ```

use std::collections::{HashMap, HashSet};

#[allow(unused_imports)]
use log::{debug, error, info, trace};

use bitcoin::{Transaction, Txid};

use electrum_client::{Client, ConfigBuilder, ElectrumApi, Socks5Config};

use super::script_sync::Request;
use super::*;
use crate::database::{BatchDatabase, Database};
use crate::error::Error;
use crate::{BlockTime, FeeRate};

/// Wrapper over an Electrum Client that implements the required blockchain traits
///
/// ## Example
/// See the [`blockchain::electrum`](crate::blockchain::electrum) module for a usage example.
pub struct ElectrumBlockchain {
    client: Client,
    stop_gap: usize,
}

impl std::convert::From<Client> for ElectrumBlockchain {
    fn from(client: Client) -> Self {
        ElectrumBlockchain {
            client,
            stop_gap: 20,
        }
    }
}

impl Blockchain for ElectrumBlockchain {
    fn get_capabilities(&self) -> HashSet<Capability> {
        vec![
            Capability::FullHistory,
            Capability::GetAnyTx,
            Capability::AccurateFees,
        ]
        .into_iter()
        .collect()
    }

    fn broadcast(&self, tx: &Transaction) -> Result<(), Error> {
        Ok(self.client.transaction_broadcast(tx).map(|_| ())?)
    }

    fn estimate_fee(&self, target: usize) -> Result<FeeRate, Error> {
        Ok(FeeRate::from_btc_per_kvb(
            self.client.estimate_fee(target)? as f32
        ))
    }
}

impl StatelessBlockchain for ElectrumBlockchain {}

impl GetHeight for ElectrumBlockchain {
    fn get_height(&self) -> Result<u32, Error> {
        // TODO: unsubscribe when added to the client, or is there a better call to use here?

        Ok(self
            .client
            .block_headers_subscribe()
            .map(|data| data.height as u32)?)
    }
}

impl GetTx for ElectrumBlockchain {
    fn get_tx(&self, txid: &Txid) -> Result<Option<Transaction>, Error> {
        Ok(self.client.transaction_get(txid).map(Option::Some)?)
    }
}

impl WalletSync for ElectrumBlockchain {
    fn wallet_setup<D: BatchDatabase>(
        &self,
        database: &mut D,
        _progress_update: Box<dyn Progress>,
    ) -> Result<(), Error> {
        let mut request = script_sync::start(database, self.stop_gap)?;
        let mut block_times = HashMap::<u32, u32>::new();
        let mut txid_to_height = HashMap::<Txid, u32>::new();
        let mut tx_cache = TxCache::new(database, &self.client);
        let chunk_size = self.stop_gap;
        // The electrum server has been inconsistent somehow in its responses during sync. For
        // example, we do a batch request of transactions and the response contains less
        // tranascations than in the request. This should never happen but we don't want to panic.
        let electrum_goof = || Error::Generic("electrum server misbehaving".to_string());

        let batch_update = loop {
            request = match request {
                Request::Script(script_req) => {
                    let scripts = script_req.request().take(chunk_size);
                    let txids_per_script: Vec<Vec<_>> = self
                        .client
                        .batch_script_get_history(scripts)
                        .map_err(Error::Electrum)?
                        .into_iter()
                        .map(|txs| {
                            txs.into_iter()
                                .map(|tx| {
                                    let tx_height = match tx.height {
                                        none if none <= 0 => None,
                                        height => {
                                            txid_to_height.insert(tx.tx_hash, height as u32);
                                            Some(height as u32)
                                        }
                                    };
                                    (tx.tx_hash, tx_height)
                                })
                                .collect()
                        })
                        .collect();

                    script_req.satisfy(txids_per_script)?
                }

                Request::Conftime(conftime_req) => {
                    // collect up to chunk_size heights to fetch from electrum
                    let needs_block_height = {
                        let mut needs_block_height_iter = conftime_req
                            .request()
                            .filter_map(|txid| txid_to_height.get(txid).cloned())
                            .filter(|height| block_times.get(height).is_none());
                        let mut needs_block_height = HashSet::new();

                        while needs_block_height.len() < chunk_size {
                            match needs_block_height_iter.next() {
                                Some(height) => needs_block_height.insert(height),
                                None => break,
                            };
                        }
                        needs_block_height
                    };

                    let new_block_headers = self
                        .client
                        .batch_block_header(needs_block_height.iter().cloned())?;

                    for (height, header) in needs_block_height.into_iter().zip(new_block_headers) {
                        block_times.insert(height, header.time);
                    }

                    let conftimes = conftime_req
                        .request()
                        .take(chunk_size)
                        .map(|txid| {
                            let confirmation_time = txid_to_height
                                .get(txid)
                                .map(|height| {
                                    let timestamp =
                                        *block_times.get(height).ok_or_else(electrum_goof)?;
                                    Result::<_, Error>::Ok(BlockTime {
                                        height: *height,
                                        timestamp: timestamp.into(),
                                    })
                                })
                                .transpose()?;
                            Ok(confirmation_time)
                        })
                        .collect::<Result<_, Error>>()?;

                    conftime_req.satisfy(conftimes)?
                }
                Request::Tx(tx_req) => {
                    let needs_full = tx_req.request().take(chunk_size);
                    tx_cache.save_txs(needs_full.clone())?;
                    let full_transactions = needs_full
                        .map(|txid| tx_cache.get(*txid).ok_or_else(electrum_goof))
                        .collect::<Result<Vec<_>, _>>()?;
                    let input_txs = full_transactions.iter().flat_map(|tx| {
                        tx.input
                            .iter()
                            .filter(|input| !input.previous_output.is_null())
                            .map(|input| &input.previous_output.txid)
                    });
                    tx_cache.save_txs(input_txs)?;

                    let full_details = full_transactions
                        .into_iter()
                        .map(|tx| {
                            let mut input_index = 0usize;
                            let prev_outputs = tx
                                .input
                                .iter()
                                .map(|input| {
                                    if input.previous_output.is_null() {
                                        return Ok(None);
                                    }
                                    let prev_tx = tx_cache
                                        .get(input.previous_output.txid)
                                        .ok_or_else(electrum_goof)?;
                                    let txout = prev_tx
                                        .output
                                        .get(input.previous_output.vout as usize)
                                        .ok_or_else(electrum_goof)?;
                                    input_index += 1;
                                    Ok(Some(txout.clone()))
                                })
                                .collect::<Result<Vec<_>, Error>>()?;
                            Ok((prev_outputs, tx))
                        })
                        .collect::<Result<Vec<_>, Error>>()?;

                    tx_req.satisfy(full_details)?
                }
                Request::Finish(batch_update) => break batch_update,
            }
        };

        database.commit_batch(batch_update)?;
        Ok(())
    }
}

struct TxCache<'a, 'b, D> {
    db: &'a D,
    client: &'b Client,
    cache: HashMap<Txid, Transaction>,
}

impl<'a, 'b, D: Database> TxCache<'a, 'b, D> {
    fn new(db: &'a D, client: &'b Client) -> Self {
        TxCache {
            db,
            client,
            cache: HashMap::default(),
        }
    }
    fn save_txs<'c>(&mut self, txids: impl Iterator<Item = &'c Txid>) -> Result<(), Error> {
        let mut need_fetch = vec![];
        for txid in txids {
            if self.cache.get(txid).is_some() {
                continue;
            } else if let Some(transaction) = self.db.get_raw_tx(txid)? {
                self.cache.insert(*txid, transaction);
            } else {
                need_fetch.push(txid);
            }
        }

        if !need_fetch.is_empty() {
            let txs = self
                .client
                .batch_transaction_get(need_fetch.clone())
                .map_err(Error::Electrum)?;
            for (tx, _txid) in txs.into_iter().zip(need_fetch) {
                debug_assert_eq!(*_txid, tx.txid());
                self.cache.insert(tx.txid(), tx);
            }
        }

        Ok(())
    }

    fn get(&self, txid: Txid) -> Option<Transaction> {
        self.cache.get(&txid).map(Clone::clone)
    }
}

/// Configuration for an [`ElectrumBlockchain`]
#[derive(Debug, serde::Deserialize, serde::Serialize, Clone, PartialEq)]
pub struct ElectrumBlockchainConfig {
    /// URL of the Electrum server (such as ElectrumX, Esplora, BWT) may start with `ssl://` or `tcp://` and include a port
    ///
    /// eg. `ssl://electrum.blockstream.info:60002`
    pub url: String,
    /// URL of the socks5 proxy server or a Tor service
    pub socks5: Option<String>,
    /// Request retry count
    pub retry: u8,
    /// Request timeout (seconds)
    pub timeout: Option<u8>,
    /// Stop searching addresses for transactions after finding an unused gap of this length
    pub stop_gap: usize,
}

impl ConfigurableBlockchain for ElectrumBlockchain {
    type Config = ElectrumBlockchainConfig;

    fn from_config(config: &Self::Config) -> Result<Self, Error> {
        let socks5 = config.socks5.as_ref().map(Socks5Config::new);
        let electrum_config = ConfigBuilder::new()
            .retry(config.retry)
            .timeout(config.timeout)?
            .socks5(socks5)?
            .build();

        Ok(ElectrumBlockchain {
            client: Client::from_config(config.url.as_str(), electrum_config)?,
            stop_gap: config.stop_gap,
        })
    }
}

#[cfg(test)]
#[cfg(feature = "test-electrum")]
mod test {
    use std::sync::Arc;

    use super::*;
    use crate::database::MemoryDatabase;
    use crate::make_blockchain_tests;
    use crate::testutils::blockchain_tests::{BlockchainType, TestClient};
    use crate::wallet::{AddressIndex, Wallet};

    crate::bdk_blockchain_tests! {
        fn test_instance(test_client: &TestClient) -> ElectrumBlockchain {
            ElectrumBlockchain::from(Client::new(&test_client.electrsd.electrum_url).unwrap())
        }
    }

    make_blockchain_tests![
        @type BlockchainType::ElectrumBlockchain,
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
            test_taproot_script_spend,
            test_sign_taproot_core_keyspend_psbt,
            test_sign_taproot_core_scriptspend2_psbt,
            test_sign_taproot_core_scriptspend3_psbt,
        )
    ];

    fn get_factory() -> (TestClient, Arc<ElectrumBlockchain>) {
        let test_client = TestClient::default();

        let factory = Arc::new(ElectrumBlockchain::from(
            Client::new(&test_client.electrsd.electrum_url).unwrap(),
        ));

        (test_client, factory)
    }

    #[test]
    fn test_electrum_blockchain_factory() {
        let (_test_client, factory) = get_factory();

        let a = factory.build("aaaaaa", None).unwrap();
        let b = factory.build("bbbbbb", None).unwrap();

        assert_eq!(
            a.client.block_headers_subscribe().unwrap().height,
            b.client.block_headers_subscribe().unwrap().height
        );
    }

    #[test]
    fn test_electrum_blockchain_factory_sync_wallet() {
        let (mut test_client, factory) = get_factory();

        let db = MemoryDatabase::new();
        let wallet = Wallet::new(
            "wpkh(L5EZftvrYaSudiozVRzTqLcHLNDoVn7H5HSfM9BAN6tMJX8oTWz6)",
            None,
            bitcoin::Network::Regtest,
            db,
        )
        .unwrap();

        let address = wallet.get_address(AddressIndex::New).unwrap();

        let tx = testutils! {
            @tx ( (@addr address.address) => 50_000 )
        };
        test_client.receive(tx);

        factory
            .sync_wallet(&wallet, None, Default::default())
            .unwrap();

        assert_eq!(wallet.get_balance().unwrap(), 50_000);
    }
}
