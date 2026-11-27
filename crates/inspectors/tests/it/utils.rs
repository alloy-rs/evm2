use alloy_primitives::{Address, Bytes};
use colorchoice::ColorChoice;
use evm2_inspectors::tracing::{TraceWriter, TraceWriterConfig, TracingInspector};

pub use crate::compat::*;
pub(crate) use evm2::utils::u64_to_address as a;

pub fn write_traces(tracer: &TracingInspector) -> String {
    write_traces_with(tracer, TraceWriterConfig::new().color_choice(ColorChoice::Never))
}

pub fn write_traces_with(tracer: &TracingInspector, config: TraceWriterConfig) -> String {
    let mut w = TraceWriter::with_config(Vec::<u8>::new(), config);
    w.write_arena(tracer.traces()).expect("failed to write traces to Vec<u8>");
    String::from_utf8(w.into_writer()).expect("trace writer wrote invalid UTF-8")
}

pub fn print_traces(tracer: &TracingInspector) {
    // Use `println!` so that the output is captured by the test runner.
    println!("{}", write_traces_with(tracer, TraceWriterConfig::new()));
}

pub fn deploy_contract(
    evm: &mut TestEvm,
    code: Bytes,
    deployer: Address,
    spec: SpecId,
) -> DeployResult {
    evm.ctx.spec = spec;
    let value = evm.ctx.tx.value;
    let result = evm
        .inspect_tx_commit(TxEnv {
            caller: deployer,
            gas_limit: 1000000,
            kind: TransactTo::Create,
            data: code,
            value,
            nonce: evm.ctx.tx.nonce,
            ..Default::default()
        })
        .expect("Expect to be executed");
    evm.ctx.tx.nonce += 1;
    DeployResult { result }
}

pub fn inspect_deploy_contract<I: InspectorSlot>(
    evm: &mut TestEvmWithInspector<I>,
    code: Bytes,
    deployer: Address,
    spec: SpecId,
) -> DeployResult {
    evm.ctx.spec = spec;
    let value = evm.ctx.tx.value;
    let result = evm
        .inspect_tx_commit(TxEnv {
            caller: deployer,
            gas_limit: 1000000,
            kind: TransactTo::Create,
            data: code,
            value,
            nonce: evm.ctx.tx.nonce,
            ..Default::default()
        })
        .expect("Expect to be executed");
    evm.ctx.tx.nonce += 1;
    DeployResult { result }
}

pub(crate) mod frames {
    //! Shared setup for native frame tracing regressions.

    use super::*;
    use alloy_consensus::{TxEip8141, transaction::Recovered};
    use alloy_eips::eip8141::{Frame, FrameLimits, FrameMode, FrameSignature, SignatureScheme};
    use alloy_primitives::U256;
    use evm2::{
        BaseEvmTypes, Evm, Precompiles,
        env::BlockEnvExt,
        ethereum::{TxEnvelope, ethereum_tx_registry},
        evm::InMemoryDB,
    };
    use k256::ecdsa::SigningKey;

    pub(crate) const APPROVE_BOTH: &[u8] = &[0x60, 3, 0x5f, 0x5f, 0xaa];
    pub(crate) const STORE_AND_LOG: &[u8] = &[0x60, 1, 0x5f, 0x55, 0x5f, 0x5f, 0xa0, 0x00];
    pub(crate) const REVERT: &[u8] = &[0x5f, 0x5f, 0xfd];

    /// Builds a frame with the tracing suite's execution and state gas budgets.
    pub(crate) fn frame(mode: FrameMode, target: Address, flags: u8) -> Frame {
        Frame {
            mode,
            target: target.into(),
            flags,
            limits: FrameLimits { execution: 100_000, state: 300_000 },
            ..Default::default()
        }
    }

    /// Wraps a frame payload with a deliberately different recovered signer.
    pub(crate) fn transaction(frames: Vec<Frame>) -> Recovered<TxEnvelope> {
        Recovered::new_unchecked(
            TxEnvelope::Eip8141(payload(Address::with_last_byte(0xa0), frames)),
            Address::with_last_byte(0xff),
        )
    }

    /// Builds a frame transaction with the explicit sender and legacy nonce domain.
    pub(crate) fn payload(sender: Address, frames: Vec<Frame>) -> TxEip8141 {
        TxEip8141 { chain_id: 1, sender, nonce_keys: Some(vec![]), frames, ..Default::default() }
    }

    /// Builds a Bogota EVM with funded accounts and the supplied runtime code.
    pub(crate) fn evm(codes: &[(Address, &[u8])]) -> Evm<'static, BaseEvmTypes> {
        let mut db = InMemoryDB::default();
        for (address, code) in codes {
            db.insert_account_info(
                address,
                AccountInfo::default()
                    .with_balance(U256::from(1_000_000_000))
                    .with_code(Bytecode::new_legacy(Bytes::copy_from_slice(code))),
            );
        }
        Evm::new(
            SpecId::BOGOTA,
            BlockEnvExt::default(),
            ethereum_tx_registry(SpecId::BOGOTA),
            db,
            Precompiles::base(SpecId::BOGOTA),
        )
    }

    /// Signs a default-account verification frame with its canonical transaction hash.
    pub(crate) fn default_verification_transaction(
        data: Bytes,
    ) -> (Address, Recovered<TxEnvelope>) {
        let key = SigningKey::from_slice(&[0x11; 32]).unwrap();
        let sender =
            Address::from_raw_public_key(&key.verifying_key().to_sec1_point(false).as_bytes()[1..]);
        let mut tx = payload(sender, vec![Frame { data, ..frame(FrameMode::Verify, sender, 3) }]);
        tx.signatures.push(FrameSignature {
            scheme: SignatureScheme::Secp256k1,
            signature: vec![0; 65].into(),
            ..Default::default()
        });
        let (sig, recid) = key.sign_prehash_recoverable(tx.signature_hash().as_slice());
        let mut signature = vec![recid.to_byte()];
        signature.extend_from_slice(sig.to_bytes().as_slice());
        tx.signatures[0].signature = signature.into();
        (sender, Recovered::new_unchecked(TxEnvelope::Eip8141(tx), Address::ZERO))
    }
}
