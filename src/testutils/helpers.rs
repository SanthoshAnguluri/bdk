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
#![allow(missing_docs)]
#![allow(unused)]

use std::str::FromStr;

use bitcoin::{Address, Network, OutPoint, Transaction, TxIn, TxOut, Txid};

use crate::{
    database::{AnyDatabase, BatchOperations, MemoryDatabase},
    testutils, BlockTime, KeychainKind, LocalUtxo, TransactionDetails, Wallet,
};

use super::TestIncomingTx;

/// Populate a test database with a `TestIncomingTx`, as if we had found the tx with a `sync`.
/// This is a hidden function, only useful for `DataBase` unit testing.
pub fn populate_test_db(
    db: &mut impl BatchOperations,
    tx_meta: TestIncomingTx,
    current_height: u32,
    is_coinbase: bool,
) -> Txid {
    // Ignore `tx_meta` inputs while creating a coinbase transaction
    let input = if is_coinbase {
        // `TxIn::default()` creates a coinbase input, by definition.
        vec![TxIn::default()]
    } else {
        tx_meta
            .input
            .iter()
            .map(|test_input| {
                let mut txin = TxIn {
                    previous_output: OutPoint {
                        txid: test_input.txid,
                        vout: test_input.vout,
                    },
                    ..Default::default()
                };

                if let Some(seq) = test_input.sequence {
                    txin.sequence = seq;
                }
                txin
            })
            .collect()
    };

    let output = tx_meta
        .output
        .iter()
        .map(|out_meta| TxOut {
            value: out_meta.value,
            script_pubkey: Address::from_str(&out_meta.to_address)
                .unwrap()
                .script_pubkey(),
        })
        .collect();

    let tx = Transaction {
        version: 1,
        lock_time: 0,
        input,
        output,
    };

    let txid = tx.txid();
    let confirmation_time = tx_meta.min_confirmations.map(|conf| BlockTime {
        height: current_height.checked_sub(conf as u32).unwrap(),
        timestamp: 0,
    });

    let tx_details = TransactionDetails {
        transaction: Some(tx.clone()),
        txid,
        fee: Some(0),
        received: 0,
        sent: 0,
        confirmation_time,
    };

    db.set_tx(&tx_details).unwrap();
    for (vout, out) in tx.output.iter().enumerate() {
        db.set_utxo(&LocalUtxo {
            txout: out.clone(),
            outpoint: OutPoint {
                txid,
                vout: vout as u32,
            },
            keychain: KeychainKind::External,
            is_spent: false,
        })
        .unwrap();
    }

    txid
}

#[doc(hidden)]
#[cfg(test)]
/// Return a fake wallet that appears to be funded for testing.
pub(crate) fn get_funded_wallet(
    descriptor: &str,
) -> (Wallet<AnyDatabase>, (String, Option<String>), bitcoin::Txid) {
    let descriptors = testutils!(@descriptors (descriptor));
    let wallet = Wallet::new(
        &descriptors.0,
        None,
        Network::Regtest,
        AnyDatabase::Memory(MemoryDatabase::new()),
    )
    .unwrap();

    let funding_address_kix = 0;

    let tx_meta = testutils! {
            @tx ( (@external descriptors, funding_address_kix) => 50_000 ) (@confirmations 1)
    };

    wallet
        .database_mut()
        .set_script_pubkey(
            &bitcoin::Address::from_str(&tx_meta.output.get(0).unwrap().to_address)
                .unwrap()
                .script_pubkey(),
            KeychainKind::External,
            funding_address_kix,
        )
        .unwrap();
    wallet
        .database_mut()
        .set_last_index(KeychainKind::External, funding_address_kix)
        .unwrap();

    let txid = populate_test_db(&mut *wallet.database_mut(), tx_meta, 100, false);

    (wallet, descriptors, txid)
}

#[macro_export]
#[doc(hidden)]
macro_rules! run_tests_with_init {
(@init $fn_name:ident(), @tests ( $($x:tt) , + $(,)? )) => {
    $(
        #[test]
        fn $x()
        {
        $crate::database::test::$x($fn_name());
        }
    )+
    };
}

#[macro_export]
#[doc(hidden)]
/// Macro for getting a wallet for use in a doctest
macro_rules! doctest_wallet {
    () => {{
        use $crate::testutils::helpers::populate_test_db;
        use $crate::bitcoin::Network;
        use $crate::database::MemoryDatabase;
        use $crate::testutils;
        let descriptor = "wpkh(cVpPVruEDdmutPzisEsYvtST1usBR3ntr8pXSyt6D2YYqXRyPcFW)";
        let descriptors = testutils!(@descriptors (descriptor) (descriptor));

        let mut db = MemoryDatabase::new();
        populate_test_db(
            &mut db,
            testutils! {
                @tx ( (@external descriptors, 0) => 500_000 ) (@confirmations 1)
            },
            100,
            false
        );

        $crate::Wallet::new(
            &descriptors.0,
            descriptors.1.as_ref(),
            Network::Regtest,
            db
        )
        .unwrap()
    }}
}
