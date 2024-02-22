use crate as pallet_relayer_fees;
use crate::{
    message,
    withdrawal::{Key, Signature, WithdrawalInputData, WithdrawalProof},
    Nonce, Pallet, RelayerFees,
};
use alloy_primitives::hex;
use codec::{Decode, Encode};
use ethereum_trie::{keccak::KeccakHasher, MemoryDB};
use frame_support::{
    crypto::ecdsa::ECDSAExt,
    parameter_types,
    traits::{ConstU32, ConstU64, Get},
};
use frame_system::EnsureRoot;
use ismp::{
    consensus::{
        ConsensusClient, ConsensusClientId, StateCommitment, StateMachineClient,
        StateMachineHeight, StateMachineId, VerifiedCommitments,
    },
    error::Error,
    host::{IsmpHost, StateMachine},
    messaging::Proof,
    module::IsmpModule,
    router::{IsmpRouter, Post, Request},
    util::{hash_post_response, hash_request},
};
use pallet_ismp::{
    dispatcher::FeeMetadata,
    host::Host,
    mocks::{
        mocks::{set_timestamp, MockModule},
        ExistentialDeposit,
    },
    primitives::{HashAlgorithm, SubstrateStateProof},
    RequestCommitments, RequestReceipts, ResponseCommitments, ResponseReceipt, ResponseReceipts,
};
use sp_core::{crypto::AccountId32, Pair, H160, H256, U256};
use sp_runtime::{
    traits::{IdentityLookup, Keccak256},
    BuildStorage,
};
use sp_trie::LayoutV0;
use std::time::Duration;
use substrate_state_machine::SubstrateStateMachine;
use trie_db::{Recorder, Trie, TrieDBBuilder, TrieDBMutBuilder, TrieMut};

use frame_support::derive_impl;
use ismp::host::Ethereum;
use ismp_bsc_pos::BSC_CONSENSUS_ID;
use ismp_sync_committee::{constants::sepolia::Sepolia, BEACON_CONSENSUS_ID};
use pallet_ismp::{
    dispatcher::LeafMetadata,
    mocks::mocks::{MOCK_CONSENSUS_CLIENT_ID, MOCK_CONSENSUS_STATE_ID},
    primitives::LeafIndexAndPos,
};

type Block = frame_system::mocking::MockBlock<Test>;

frame_support::construct_runtime!(
    pub enum Test {
        System: frame_system::{Pallet, Call, Config<T>, Storage, Event<T>},
        Timestamp: pallet_timestamp::{Pallet, Call, Storage, Inherent},
        Ismp: pallet_ismp::{Pallet, Storage, Call, Event<T>},
        Balances: pallet_balances,
        PalletRelayerFees: pallet_relayer_fees,
        StateMachineManager: state_machine_manager
    }
);

pub struct StateMachineProvider;

impl Get<StateMachine> for StateMachineProvider {
    fn get() -> StateMachine {
        StateMachine::Kusama(100)
    }
}

#[derive(Default)]
pub struct MockConsensusClient;

impl ConsensusClient for MockConsensusClient {
    fn verify_consensus(
        &self,
        _host: &dyn IsmpHost,
        _cs_id: ismp::consensus::ConsensusStateId,
        _trusted_consensus_state: Vec<u8>,
        _proof: Vec<u8>,
    ) -> Result<(Vec<u8>, VerifiedCommitments), Error> {
        Ok(Default::default())
    }

    fn verify_fraud_proof(
        &self,
        _host: &dyn IsmpHost,
        _trusted_consensus_state: Vec<u8>,
        _proof_1: Vec<u8>,
        _proof_2: Vec<u8>,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn consensus_client_id(&self) -> ConsensusClientId {
        MOCK_CONSENSUS_CLIENT_ID
    }

    fn state_machine(&self, _id: StateMachine) -> Result<Box<dyn StateMachineClient>, Error> {
        Ok(Box::new(SubstrateStateMachine::<Test>::default()))
    }
}

#[derive_impl(frame_system::config_preludes::ParaChainDefaultConfig as frame_system::DefaultConfig)]
impl frame_system::Config for Test {
    type BaseCallFilter = frame_support::traits::Everything;
    type RuntimeOrigin = RuntimeOrigin;
    type RuntimeCall = RuntimeCall;
    type Hash = H256;
    type Hashing = Keccak256;
    type AccountId = AccountId32;
    type Lookup = IdentityLookup<Self::AccountId>;
    type RuntimeEvent = RuntimeEvent;
    type BlockHashCount = ConstU64<250>;
    type DbWeight = ();
    type BlockWeights = ();
    type BlockLength = ();
    type Version = ();
    type Nonce = u64;
    type Block = Block;
    type PalletInfo = PalletInfo;
    type AccountData = pallet_balances::AccountData<pallet_ismp::mocks::Balance>;
    type OnNewAccount = ();
    type OnKilledAccount = ();
    type SystemWeightInfo = ();
    type SS58Prefix = ();
    type OnSetCode = ();
    type MaxConsumers = ConstU32<16>;
}

impl pallet_timestamp::Config for Test {
    type Moment = u64;
    type OnTimestampSet = ();
    type MinimumPeriod = ConstU64<1>;
    type WeightInfo = ();
}

impl pallet_balances::Config for Test {
    /// The ubiquitous event type.
    type RuntimeEvent = RuntimeEvent;
    type RuntimeHoldReason = RuntimeHoldReason;
    type RuntimeFreezeReason = RuntimeFreezeReason;
    type WeightInfo = pallet_balances::weights::SubstrateWeight<Test>;
    /// The type for recording an account's balance.
    type Balance = pallet_ismp::mocks::Balance;
    type DustRemoval = ();
    type ExistentialDeposit = ExistentialDeposit;
    type AccountStore = System;
    type ReserveIdentifier = [u8; 8];
    type FreezeIdentifier = ();
    type MaxLocks = ConstU32<50>;
    type MaxReserves = ConstU32<50>;
    type MaxHolds = ConstU32<1>;
    type MaxFreezes = ();
}

parameter_types! {
    pub const Coprocessor: Option<StateMachine> = None;
}

impl pallet_ismp::Config for Test {
    type RuntimeEvent = RuntimeEvent;
    const INDEXING_PREFIX: &'static [u8] = b"ISMP";
    type AdminOrigin = EnsureRoot<AccountId32>;
    type HostStateMachine = pallet_ismp::mocks::StateMachineProvider;
    type TimeProvider = Timestamp;
    type Coprocessor = pallet_ismp::mocks::Coprocessor;
    type Router = pallet_ismp::mocks::ModuleRouter;
    type ConsensusClients = (
        MockConsensusClient,
        ismp_sync_committee::SyncCommitteeConsensusClient<Host<Test>, Sepolia>,
        ismp_bsc_pos::BscClient<Host<Test>>,
    );
    type WeightInfo = ();
    type WeightProvider = ();
}

impl state_machine_manager::Config for Test {
    type RuntimeEvent = RuntimeEvent;
}

impl pallet_relayer_fees::Config for Test {}

#[derive(Default)]
pub struct ModuleRouter;

impl IsmpRouter for ModuleRouter {
    fn module_for_id(&self, _bytes: Vec<u8>) -> Result<Box<dyn IsmpModule>, ismp::error::Error> {
        Ok(Box::new(MockModule))
    }
}

pub(crate) fn new_test_ext() -> sp_io::TestExternalities {
    frame_system::GenesisConfig::<pallet_ismp::mocks::Test>::default()
        .build_storage()
        .unwrap()
        .into()
}

#[test]
fn test_withdrawal_proof() {
    let mut ext = new_test_ext();
    ext.execute_with(|| {
        set_timestamp::<Test>(10_000_000_000);
        let requests = (0u64..10)
            .into_iter()
            .map(|nonce| {
                let post = Post {
                    source: StateMachine::Kusama(2000),
                    dest: StateMachine::Kusama(2001),
                    nonce,
                    from: vec![],
                    to: vec![],
                    timeout_timestamp: 0,
                    data: vec![],
                    gas_limit: 0,
                };
                hash_request::<Host<Test>>(&Request::Post(post))
            })
            .collect::<Vec<_>>();

        let responses = (0u64..10)
            .into_iter()
            .map(|nonce| {
                let post = Post {
                    source: StateMachine::Kusama(2001),
                    dest: StateMachine::Kusama(2000),
                    nonce,
                    from: vec![],
                    to: vec![],
                    timeout_timestamp: 0,
                    data: vec![],
                    gas_limit: 0,
                };
                let response = ismp::router::PostResponse {
                    post: post.clone(),
                    response: vec![0; 32],
                    timeout_timestamp: nonce,
                    gas_limit: nonce,
                };
                (
                    hash_request::<Host<Test>>(&Request::Post(post)),
                    hash_post_response::<Host<Test>>(&response),
                )
            })
            .collect::<Vec<_>>();

        let mut source_root = H256::default();

        let mut source_db = MemoryDB::<KeccakHasher>::default();
        let mut source_trie =
            TrieDBMutBuilder::<LayoutV0<KeccakHasher>>::new(&mut source_db, &mut source_root)
                .build();
        let mut dest_root = H256::default();

        let mut dest_db = MemoryDB::<KeccakHasher>::default();
        let mut dest_trie =
            TrieDBMutBuilder::<LayoutV0<KeccakHasher>>::new(&mut dest_db, &mut dest_root).build();

        // Insert requests and responses
        for request in &requests {
            let request_commitment_key = RequestCommitments::<Test>::hashed_key_for(request);
            let request_receipt_key = RequestReceipts::<Test>::hashed_key_for(request);
            let fee_metadata = FeeMetadata::<Test> { origin: [0; 32].into(), fee: 1000u128.into() };
            let leaf_meta =
                LeafMetadata { mmr: LeafIndexAndPos { leaf_index: 0, pos: 0 }, meta: fee_metadata };
            source_trie.insert(&request_commitment_key, &leaf_meta.encode()).unwrap();
            dest_trie.insert(&request_receipt_key, &vec![1u8; 32].encode()).unwrap();
        }

        for (request, response) in &responses {
            let response_commitment_key = ResponseCommitments::<Test>::hashed_key_for(response);
            let response_receipt_key = ResponseReceipts::<Test>::hashed_key_for(request);
            let fee_metadata = FeeMetadata::<Test> { origin: [0; 32].into(), fee: 1000u128.into() };
            let leaf_meta =
                LeafMetadata { mmr: LeafIndexAndPos { leaf_index: 0, pos: 0 }, meta: fee_metadata };
            source_trie.insert(&response_commitment_key, &leaf_meta.encode()).unwrap();
            let receipt = ResponseReceipt { response: *response, relayer: vec![2; 32] };
            dest_trie.insert(&response_receipt_key, &receipt.encode()).unwrap();
        }
        drop(source_trie);
        drop(dest_trie);

        let mut source_recorder = Recorder::<LayoutV0<KeccakHasher>>::default();
        let mut dest_recorder = Recorder::<LayoutV0<KeccakHasher>>::default();
        let source_trie = TrieDBBuilder::<LayoutV0<KeccakHasher>>::new(&source_db, &source_root)
            .with_recorder(&mut source_recorder)
            .build();

        let dest_trie = TrieDBBuilder::<LayoutV0<KeccakHasher>>::new(&dest_db, &dest_root)
            .with_recorder(&mut dest_recorder)
            .build();

        let mut keys = vec![];

        for (index, request) in requests.iter().enumerate() {
            if index % 2 == 0 {
                let request_commitment_key = RequestCommitments::<Test>::hashed_key_for(request);
                let request_receipt_key = RequestReceipts::<Test>::hashed_key_for(request);
                source_trie.get(&request_commitment_key).unwrap();
                dest_trie.get(&request_receipt_key).unwrap();
                keys.push(Key::Request(*request));
            }
        }

        for (index, (request, response)) in responses.iter().enumerate() {
            if index % 2 == 0 {
                let response_commitment_key = ResponseCommitments::<Test>::hashed_key_for(response);
                let response_receipt_key = ResponseReceipts::<Test>::hashed_key_for(request);
                source_trie.get(&response_commitment_key).unwrap();
                dest_trie.get(&response_receipt_key).unwrap();
                keys.push(Key::Response {
                    response_commitment: *response,
                    request_commitment: *request,
                });
            }
        }

        let source_keys_proof =
            source_recorder.drain().into_iter().map(|f| f.data).collect::<Vec<_>>();
        let dest_keys_proof = dest_recorder.drain().into_iter().map(|f| f.data).collect::<Vec<_>>();

        let source_state_proof =
            SubstrateStateProof { hasher: HashAlgorithm::Keccak, storage_proof: source_keys_proof };

        let dest_state_proof =
            SubstrateStateProof { hasher: HashAlgorithm::Keccak, storage_proof: dest_keys_proof };

        let host = Host::<Test>::default();
        host.store_state_machine_commitment(
            StateMachineHeight {
                id: StateMachineId {
                    state_id: StateMachine::Kusama(2000),
                    consensus_state_id: MOCK_CONSENSUS_STATE_ID,
                },
                height: 1,
            },
            StateCommitment { timestamp: 100, overlay_root: None, state_root: source_root },
        )
        .unwrap();

        host.store_state_machine_commitment(
            StateMachineHeight {
                id: StateMachineId {
                    state_id: StateMachine::Kusama(2001),
                    consensus_state_id: MOCK_CONSENSUS_STATE_ID,
                },
                height: 1,
            },
            StateCommitment { timestamp: 100, overlay_root: None, state_root: dest_root },
        )
        .unwrap();

        host.store_state_machine_update_time(
            StateMachineHeight {
                id: StateMachineId {
                    state_id: StateMachine::Kusama(2000),
                    consensus_state_id: MOCK_CONSENSUS_STATE_ID,
                },
                height: 1,
            },
            Duration::from_secs(100),
        )
        .unwrap();

        host.store_state_machine_update_time(
            StateMachineHeight {
                id: StateMachineId {
                    state_id: StateMachine::Kusama(2001),
                    consensus_state_id: MOCK_CONSENSUS_STATE_ID,
                },
                height: 1,
            },
            Duration::from_secs(100),
        )
        .unwrap();
        host.store_consensus_state(MOCK_CONSENSUS_STATE_ID, Default::default()).unwrap();

        host.store_consensus_state_id(MOCK_CONSENSUS_STATE_ID, MOCK_CONSENSUS_CLIENT_ID)
            .unwrap();

        host.store_unbonding_period(MOCK_CONSENSUS_STATE_ID, 10_000_000_000).unwrap();

        host.store_challenge_period(MOCK_CONSENSUS_STATE_ID, 0).unwrap();

        let withdrawal_proof = WithdrawalProof {
            commitments: keys,
            source_proof: Proof {
                height: StateMachineHeight {
                    id: StateMachineId {
                        state_id: StateMachine::Kusama(2000),
                        consensus_state_id: MOCK_CONSENSUS_STATE_ID,
                    },
                    height: 1,
                },
                proof: source_state_proof.encode(),
            },
            dest_proof: Proof {
                height: StateMachineHeight {
                    id: StateMachineId {
                        state_id: StateMachine::Kusama(2001),
                        consensus_state_id: MOCK_CONSENSUS_STATE_ID,
                    },
                    height: 1,
                },
                proof: dest_state_proof.encode(),
            },
        };

        Pallet::<Test>::accumulate_fees(RuntimeOrigin::none(), withdrawal_proof).unwrap();

        assert_eq!(
            RelayerFees::<Test>::get(StateMachine::Kusama(2000), vec![1; 32]),
            U256::from(5000u128)
        );
        assert_eq!(
            RelayerFees::<Test>::get(StateMachine::Kusama(2000), vec![2; 32]),
            U256::from(5000u128)
        );
    })
}

#[test]
fn test_withdrawal_fees() {
    let mut ext = new_test_ext();
    ext.execute_with(|| {
        let pair = sp_core::ecdsa::Pair::from_seed_slice(H256::random().as_bytes()).unwrap();
        let address = pair.public().to_eth_address().unwrap();
        RelayerFees::<Test>::insert(
            StateMachine::Kusama(2000),
            address.to_vec(),
            U256::from(5000u128),
        );
        let message = message(0, StateMachine::Kusama(2000), 2000u128.into());
        let signature = pair.sign_prehashed(&message).0.to_vec();

        let withdrawal_input = WithdrawalInputData {
            signature: Signature::Ethereum { address: address.to_vec(), signature },
            dest_chain: StateMachine::Kusama(2000),
            amount: U256::from(2000u128),
            gas_limit: 10_000_000,
        };

        Pallet::<Test>::withdraw_fees(RuntimeOrigin::none(), withdrawal_input.clone()).unwrap();
        assert_eq!(
            RelayerFees::<Test>::get(StateMachine::Kusama(2000), address.to_vec()),
            3_000u128.into()
        );

        assert_eq!(Nonce::<Test>::get(address.to_vec()), 1);

        assert!(
            Pallet::<Test>::withdraw_fees(RuntimeOrigin::none(), withdrawal_input.clone()).is_err()
        );
    })
}

#[test]
fn test_evm_accumulate_fees() {
    let mut ext = new_test_ext();
    ext.execute_with(|| {
        set_timestamp::<Test>(10_000_000_000);
        let bsc_root = H256::from_slice(&hex::decode("5e7239f000b1416b8230416ada9d39c342979aa5746172a86df00dda9fd221c9").unwrap());
        let op_root = H256::from_slice(&hex::decode("6dfbb6ec490b26ca38796ecf291ff20a6d50cc8261b0d85f27e0962a6730661e").unwrap());

        let claim_proof = WithdrawalProof::decode(&mut &*claim_proof).unwrap();
        let host = Host::<Test>::default();
        host.store_state_machine_commitment(
            claim_proof.source_proof.height,
            StateCommitment { timestamp: 100, overlay_root: None, state_root: bsc_root },
        )
            .unwrap();

        host.store_state_machine_commitment(
            claim_proof.dest_proof.height,
            StateCommitment { timestamp: 100, overlay_root: None, state_root: op_root },
        )
            .unwrap();

        host.store_state_machine_update_time(
            claim_proof.source_proof.height,
            Duration::from_secs(100),
        )
            .unwrap();

        host.store_state_machine_update_time(
            claim_proof.dest_proof.height,
            Duration::from_secs(100),
        )
            .unwrap();

        let op_host = H160::from(hex!("6bb05F1997396eC1A4A3040f48215bbC101ab7b6"));
        let bsc_host =  H160::from(hex!("C0291b0eD2E44100d1D77d9cEeeE0535B26AA45C"));
        let bsc_consensus_state = ismp_bsc_pos::ConsensusState {
            current_validators: vec![],
            next_validators: None,
            finalized_height: 0,
            finalized_hash: Default::default(),
            current_epoch: 0,
            ismp_contract_address: bsc_host,
        };
        let sync_committee_consensus_state = ismp_sync_committee::types::ConsensusState {
            frozen_height: None,
            light_client_state: Default::default(),
            ismp_contract_addresses: vec![(StateMachine::Ethereum(Ethereum::Optimism), op_host)].into_iter().collect(),
            l2_oracle_address: Default::default(),
            rollup_core_address: Default::default(),
        };
        host.store_consensus_state(claim_proof.source_proof.height.id.consensus_state_id, bsc_consensus_state.encode()).unwrap();
        host.store_consensus_state(claim_proof.dest_proof.height.id.consensus_state_id, sync_committee_consensus_state.encode()).unwrap();

        host.store_consensus_state_id(claim_proof.source_proof.height.id.consensus_state_id, BSC_CONSENSUS_ID)
            .unwrap();

        host.store_consensus_state_id(claim_proof.dest_proof.height.id.consensus_state_id, BEACON_CONSENSUS_ID)
            .unwrap();

        host.store_unbonding_period(claim_proof.source_proof.height.id.consensus_state_id, 10_000_000_000).unwrap();

        host.store_challenge_period(claim_proof.source_proof.height.id.consensus_state_id, 0).unwrap();

        host.store_unbonding_period(claim_proof.dest_proof.height.id.consensus_state_id, 10_000_000_000).unwrap();

        host.store_challenge_period(claim_proof.dest_proof.height.id.consensus_state_id, 0).unwrap();

        Pallet::<Test>::accumulate_fees(RuntimeOrigin::none(), claim_proof.clone()).unwrap();

        assert_eq!(
            RelayerFees::<Test>::get(StateMachine::Bsc, vec![125, 114, 152, 63, 237, 193, 243, 50, 229, 80, 6, 254, 162, 162, 175, 193, 72, 246, 97, 66]),
            U256::from(50_000_000_000_000_000_000u128)
        );

        assert!(Pallet::<Test>::accumulate_fees(RuntimeOrigin::none(), claim_proof.clone()).is_err());
    })
}