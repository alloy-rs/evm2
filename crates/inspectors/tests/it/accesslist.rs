//! Accesslist tests

use crate::utils::{AccountInfo, Bytecode, CacheDB, Context, EmptyDB, TransactTo, TxEnv};
use alloy_consensus::{TxEip7702, crypto::secp256k1::sign_message};
use alloy_eip2930::{AccessList, AccessListItem};
use alloy_eips::eip7702::Authorization;
use alloy_primitives::{Address, B256, Signature, U256, address, b256, hex};
use evm2::ethereum::TxEnvelope;
use evm2_inspectors::access_list::AccessListInspector;

#[test]
fn initial_access_list_is_normalized_and_pruned() {
    let [caller, callee, lower, higher] = [0x10, 0x20, 0x30, 0x40].map(Address::with_last_byte);
    let [accessed_slot, stale_slot] = [1, 2].map(B256::with_last_byte);
    let item = |address, storage_keys| AccessListItem { address, storage_keys };
    let initial = AccessList(vec![
        item(caller, vec![]),
        item(higher, vec![stale_slot]),
        item(callee, vec![stale_slot]),
        item(higher, vec![accessed_slot, accessed_slot]),
        item(Address::with_last_byte(1), vec![]),
        item(lower, vec![]),
    ]);

    let context =
        Context::mainnet().with_db(CacheDB::<EmptyDB>::default()).modify_db_chained(|db| {
            db.insert_account_info(
                &callee,
                AccountInfo {
                    // Read slot 1 so it is re-added after pruning the callee's stale seeded slot.
                    code: Some(Bytecode::new_raw(hex!("60015400").into())),
                    ..Default::default()
                },
            );
        });
    let mut inspector = AccessListInspector::new(initial);
    let mut evm = context.build_mainnet().with_inspector(&mut inspector);
    let result = evm
        .inspect_tx(TxEnv {
            caller,
            gas_limit: 100_000,
            kind: TransactTo::Call(callee),
            ..Default::default()
        })
        .unwrap();
    assert!(result.result.is_success(), "{result:#?}");

    let expected = AccessList(vec![
        item(callee, vec![accessed_slot]),
        item(lower, vec![]),
        item(higher, vec![accessed_slot, stale_slot]),
    ]);
    assert_eq!(inspector.access_list(), expected);
    assert_eq!(inspector.into_access_list(), expected);
}

#[test]
fn test_access_list_precompile() {
    /*
    contract Storage {
       function recoverSignature() public view returns (address) {
            address r = ecrecover(bytes32(0), 0, 0, 0);
        }
    }
    */

    let code = hex!(
        "608060405234801561000f575f80fd5b5060043610610029575f3560e01c8063a53997051461002d575b5f80fd5b61003561004b565b60405161004291906100e0565b60405180910390f35b5f8060015f801b5f805f6040515f8152602001604052604051610071949392919061019a565b6020604051602081039080840390855afa158015610091573d5f803e3d5ffd5b5050506020604051035190505090565b5f73ffffffffffffffffffffffffffffffffffffffff82169050919050565b5f6100ca826100a1565b9050919050565b6100da816100c0565b82525050565b5f6020820190506100f35f8301846100d1565b92915050565b5f819050919050565b61010b816100f9565b82525050565b5f819050919050565b5f60ff82169050919050565b5f819050919050565b5f61014961014461013f84610111565b610126565b61011a565b9050919050565b6101598161012f565b82525050565b5f815f1b9050919050565b5f61018461017f61017a84610111565b61015f565b6100f9565b9050919050565b6101948161016a565b82525050565b5f6080820190506101ad5f830187610102565b6101ba6020830186610150565b6101c7604083018561018b565b6101d4606083018461018b565b9594505050505056fea26469706673582212208a19ad28dde042d3a2dd7ca8800d08fed7eb780b9778cd88f1e5ab44407532de64736f6c634300081a0033"
    );

    let account = address!("341348115259a8bf69f1f50101c227fced83bac6");
    let caller = address!("341348115259a8bf69f1f50101c227fced83bac5");

    let context =
        Context::mainnet().with_db(CacheDB::<EmptyDB>::default()).modify_db_chained(|db| {
            db.insert_account_info(
                &account,
                AccountInfo { code: Some(Bytecode::new_raw(code.into())), ..Default::default() },
            );
        });

    let mut accesslist = AccessListInspector::default();
    let mut evm = context.build_mainnet().with_inspector(&mut accesslist);
    let res = evm
        .inspect_tx(TxEnv {
            caller,
            gas_limit: 1000000,
            kind: TransactTo::Call(account),
            data: hex!("a5399705").into(),
            nonce: 0,
            ..Default::default()
        })
        .unwrap();
    assert!(res.result.is_success(), "{res:#?}");

    let erecover = address!("0x0000000000000000000000000000000000000001");
    assert!(accesslist.excluded().contains(&erecover));
    assert!(accesslist.into_access_list().is_empty());
}

#[test]
fn test_access_list_authorization_exclusions() {
    const CHAIN_ID: u64 = 1;
    const TX_CHAIN_ID: u64 = CHAIN_ID + 1;

    let authorization = |chain_id: u64, nonce| Authorization {
        chain_id: U256::from(chain_id),
        address: Address::ZERO,
        nonce,
    };
    let sign = |authorization: Authorization, secret| {
        let signature = sign_message(B256::with_last_byte(secret), authorization.signature_hash());
        authorization.into_signed(signature.unwrap())
    };
    let current_chain = sign(authorization(CHAIN_ID, 0), 1);
    let zero_chain = sign(authorization(0, 0), 2);
    let wrong_chain = sign(authorization(TX_CHAIN_ID, 0), 3);
    let max_nonce = sign(authorization(CHAIN_ID, u64::MAX), 4);
    let unrecoverable =
        authorization(CHAIN_ID, 0).into_signed(Signature::new(U256::ZERO, U256::ZERO, false));
    let [current_chain_authority, zero_chain_authority, wrong_chain_authority, max_nonce_authority] =
        [&current_chain, &zero_chain, &wrong_chain, &max_nonce]
            .map(|authorization| authorization.recover_authority().unwrap());

    let tx = TxEnvelope::from(TxEip7702 {
        chain_id: TX_CHAIN_ID,
        authorization_list: vec![current_chain, zero_chain, wrong_chain, max_nonce, unrecoverable],
        ..Default::default()
    });

    let inspector = AccessListInspector::default().with_excluded_from_tx(&tx, CHAIN_ID);
    let excluded = inspector.excluded();

    assert!(excluded.contains(&current_chain_authority));
    assert!(excluded.contains(&zero_chain_authority));
    assert!(!excluded.contains(&wrong_chain_authority));
    assert!(!excluded.contains(&max_nonce_authority));
    // The unrecoverable authorization has no authority and therefore adds nothing.
    assert_eq!(excluded.len(), 2);
}

const fn a(n: u64) -> Address {
    let mut bytes = [0; 20];
    let (_, low) = bytes.split_at_mut(12);
    let n = n.to_be_bytes();
    let mut i = 0;
    while i < 8 {
        low[i] = n[i];
        i += 1;
    }
    Address::new(bytes)
}

/// A call run through `go-ethereum`'s `eth_createAccessList`, and the list it returned.
struct AccessListCase {
    name: &'static str,
    /// Address, code and balance.
    accounts: &'static [(Address, &'static [u8], u64)],
    to: Option<Address>,
    input: &'static [u8],
    gas_limit: u64,
    /// The access list of the request.
    seed: &'static [(Address, &'static [B256])],
    expected: &'static [(Address, &'static [B256])],
}

fn access_list(items: &[(Address, &[B256])]) -> AccessList {
    AccessList(
        items
            .iter()
            .map(|&(address, storage_keys)| AccessListItem {
                address,
                storage_keys: storage_keys.to_vec(),
            })
            .collect(),
    )
}

/// Lists returned by `go-ethereum` a0cba163 (`geth --dev`, Osaka) for the same calls.
#[test]
fn access_list_matches_geth() {
    let sender = a(0x1001);
    for case in CASES {
        let mut db = CacheDB::<EmptyDB>::default();
        db.insert_account_info(
            &sender,
            AccountInfo { balance: U256::from(10u128.pow(18)), ..Default::default() },
        );
        for &(address, code, balance) in case.accounts {
            db.insert_account_info(
                &address,
                AccountInfo {
                    balance: U256::from(balance),
                    code: Some(Bytecode::new_raw(code.to_vec().into())),
                    ..Default::default()
                },
            );
        }
        let context = Context::mainnet().with_db(db);

        // Like `go-ethereum`, run again with the list found so far until it stops growing.
        let mut list = access_list(case.seed);
        let actual = loop {
            let mut inspector = AccessListInspector::new(list.clone());
            let mut evm = context.clone().build_mainnet().with_inspector(&mut inspector);
            evm.inspect_tx(TxEnv {
                caller: sender,
                gas_limit: case.gas_limit,
                kind: case.to.map_or(TransactTo::Create, TransactTo::Call),
                data: case.input.to_vec().into(),
                nonce: 0,
                access_list: list.clone(),
                ..Default::default()
            })
            .unwrap();
            let next = inspector.access_list();
            if next == list {
                break next;
            }
            list = next;
        };

        assert_eq!(actual, access_list(case.expected), "{}", case.name);
    }
}

const CASES: &[AccessListCase] = &[
    AccessListCase {
        name: "call_underflow_2",
        accounts: &[(a(0xc1), &hex!("73000000000000000000000000000000000000b0016000f1"), 0)],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[],
    },
    AccessListCase {
        name: "call_underflow_4",
        accounts: &[(
            a(0xc1),
            &hex!("60006000600073000000000000000000000000000000000000b001f1"),
            0,
        )],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[],
    },
    AccessListCase {
        name: "call_underflow_6",
        accounts: &[(
            a(0xc1),
            &hex!("600060006000600073000000000000000000000000000000000000b0016000f1"),
            0,
        )],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[(a(0xb001), &[])],
    },
    AccessListCase {
        name: "call_underflow_5",
        accounts: &[(
            a(0xc1),
            &hex!("60006000600073000000000000000000000000000000000000b0016000f1"),
            0,
        )],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[(a(0xb001), &[])],
    },
    AccessListCase {
        name: "staticcall_underflow_4",
        accounts: &[(
            a(0xc1),
            &hex!("6000600073000000000000000000000000000000000000b0016000fa"),
            0,
        )],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[],
    },
    AccessListCase {
        name: "staticcall_underflow_5",
        accounts: &[(
            a(0xc1),
            &hex!("60006000600073000000000000000000000000000000000000b0016000fa"),
            0,
        )],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[(a(0xb001), &[])],
    },
    AccessListCase {
        name: "delegatecall_underflow_5",
        accounts: &[(
            a(0xc1),
            &hex!("60006000600073000000000000000000000000000000000000b0016000f4"),
            0,
        )],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[(a(0xb001), &[])],
    },
    AccessListCase {
        name: "callcode_underflow_4",
        accounts: &[(
            a(0xc1),
            &hex!("6000600073000000000000000000000000000000000000b0016000f2"),
            0,
        )],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[],
    },
    AccessListCase {
        name: "call_ok",
        accounts: &[(
            a(0xc1),
            &hex!("6000600060006000600073000000000000000000000000000000000000b00161fffff150"),
            0,
        )],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[(a(0xb001), &[])],
    },
    AccessListCase {
        name: "balance_ok",
        accounts: &[(a(0xc1), &hex!("73000000000000000000000000000000000000b0013100"), 0)],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[(a(0xb001), &[])],
    },
    AccessListCase {
        name: "balance_empty_stack",
        accounts: &[(a(0xc1), &hex!("31"), 0)],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[],
    },
    AccessListCase {
        name: "extcodecopy_underflow",
        accounts: &[(a(0xc1), &hex!("73000000000000000000000000000000000000b0013c"), 0)],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[(a(0xb001), &[])],
    },
    AccessListCase {
        name: "sload_slot",
        accounts: &[(a(0xc1), &hex!("60055400"), 0)],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[(
            a(0xc1),
            &[b256!("0x0000000000000000000000000000000000000000000000000000000000000005")],
        )],
    },
    AccessListCase {
        name: "sstore_underflow",
        accounts: &[(a(0xc1), &hex!("600755"), 0)],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[(
            a(0xc1),
            &[b256!("0x0000000000000000000000000000000000000000000000000000000000000007")],
        )],
    },
    AccessListCase {
        name: "balance_oog",
        accounts: &[(a(0xc1), &hex!("73000000000000000000000000000000000000b0013100"), 0)],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 23453,
        seed: &[],
        expected: &[(a(0xb001), &[])],
    },
    AccessListCase {
        name: "sload_oog",
        accounts: &[(a(0xc1), &hex!("60055400"), 0)],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 25353,
        seed: &[],
        expected: &[(
            a(0xc1),
            &[b256!("0x0000000000000000000000000000000000000000000000000000000000000005")],
        )],
    },
    AccessListCase {
        name: "sstore_oog_sentry",
        accounts: &[(a(0xc1), &hex!("600160095500"), 0)],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 27300,
        seed: &[],
        expected: &[(
            a(0xc1),
            &[b256!("0x0000000000000000000000000000000000000000000000000000000000000009")],
        )],
    },
    AccessListCase {
        name: "call_oog",
        accounts: &[(
            a(0xc1),
            &hex!("6000600060006000600073000000000000000000000000000000000000b00161fffff150"),
            0,
        )],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 23481,
        seed: &[],
        expected: &[(a(0xb001), &[])],
    },
    AccessListCase {
        name: "sstore_static",
        accounts: &[
            (
                a(0xc1),
                &hex!("60006000600060007300000000000000000000000000000000000000c261fffffa50"),
                0,
            ),
            (a(0xc2), &hex!("6001600355"), 0),
        ],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[(
            a(0xc2),
            &[b256!("0x0000000000000000000000000000000000000000000000000000000000000003")],
        )],
    },
    AccessListCase {
        name: "delegatecall_sload",
        accounts: &[
            (
                a(0xc1),
                &hex!("60006000600060007300000000000000000000000000000000000000c261fffff450"),
                0,
            ),
            (a(0xc2), &hex!("60045400"), 0),
        ],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[
            (
                a(0xc1),
                &[b256!("0x0000000000000000000000000000000000000000000000000000000000000004")],
            ),
            (a(0xc2), &[]),
        ],
    },
    AccessListCase {
        name: "callcode_sload",
        accounts: &[
            (
                a(0xc1),
                &hex!("600060006000600060007300000000000000000000000000000000000000c261fffff250"),
                0,
            ),
            (a(0xc2), &hex!("60045400"), 0),
        ],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[
            (
                a(0xc1),
                &[b256!("0x0000000000000000000000000000000000000000000000000000000000000004")],
            ),
            (a(0xc2), &[]),
        ],
    },
    AccessListCase {
        name: "selfdestruct",
        accounts: &[(a(0xc1), &hex!("73000000000000000000000000000000000000b001ff"), 1)],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[(a(0xb001), &[])],
    },
    AccessListCase {
        name: "selfdestruct_underflow",
        accounts: &[(a(0xc1), &hex!("ff"), 0)],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[],
    },
    AccessListCase {
        name: "excluded_sender_precompile",
        accounts: &[(
            a(0xc1),
            &hex!(
                "73000000000000000000000000000000000000100131507300000000000000000000000000000000000000c13150730000000000000000000000000000000000000002315073000000000000000000000000000000000000b0013100"
            ),
            0,
        )],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[(a(0xb001), &[])],
    },
    AccessListCase {
        name: "callcode_underflow_5",
        accounts: &[(
            a(0xc1),
            &hex!("60006000600073000000000000000000000000000000000000b0016000f2"),
            0,
        )],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[(a(0xb001), &[])],
    },
    AccessListCase {
        name: "create_tx_sload",
        accounts: &[],
        to: None,
        input: &hex!("6005545f5ff3"),
        gas_limit: 5000000,
        seed: &[],
        expected: &[(
            address!("0x3817e247023b4f489352758397040b1fd33b300a"),
            &[b256!("0x0000000000000000000000000000000000000000000000000000000000000005")],
        )],
    },
    AccessListCase {
        name: "create_nested",
        accounts: &[(
            a(0xc1),
            &hex!("606060005360056001536054600253605f600353605f60045360f3600553600660006000f050"),
            0,
        )],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[(
            address!("0x724572a7f7b8eb3aa9763a66f2511269ec3764d5"),
            &[b256!("0x0000000000000000000000000000000000000000000000000000000000000005")],
        )],
    },
    AccessListCase {
        name: "seeded",
        accounts: &[(a(0xc1), &hex!("60055400"), 0)],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[
            (
                a(0xd1),
                &[b256!("0x0000000000000000000000000000000000000000000000000000000000000009")],
            ),
            (
                a(0xc1),
                &[b256!("0x0000000000000000000000000000000000000000000000000000000000000007")],
            ),
            (
                a(0x1001),
                &[b256!("0x0000000000000000000000000000000000000000000000000000000000000001")],
            ),
            (a(0x2), &[]),
        ],
        expected: &[
            (
                a(0xc1),
                &[b256!("0x0000000000000000000000000000000000000000000000000000000000000005")],
            ),
            (
                a(0xd1),
                &[b256!("0x0000000000000000000000000000000000000000000000000000000000000009")],
            ),
        ],
    },
    AccessListCase {
        name: "reverted_frame_touch",
        accounts: &[
            (
                a(0xc1),
                &hex!("600060006000600060007300000000000000000000000000000000000000c261fffff150"),
                0,
            ),
            (a(0xc2), &hex!("73000000000000000000000000000000000000b001315060065460006000fd"), 0),
        ],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[
            (
                a(0xc2),
                &[b256!("0x0000000000000000000000000000000000000000000000000000000000000006")],
            ),
            (a(0xb001), &[]),
        ],
    },
    AccessListCase {
        name: "extcode_ops",
        accounts: &[(
            a(0xc1),
            &hex!(
                "7300000000000000000000000000000000000000e13b507300000000000000000000000000000000000000e23f506000600060007300000000000000000000000000000000000000e33c00"
            ),
            0,
        )],
        to: Some(a(0xc1)),
        input: &hex!(""),
        gas_limit: 5000000,
        seed: &[],
        expected: &[(a(0xe1), &[]), (a(0xe2), &[]), (a(0xe3), &[])],
    },
];
