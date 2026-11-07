#![allow(missing_docs)]

//! Hot-path cost of [`JitBackend::lookup_with`] for a resident program.

use alloy_primitives::{Bytes, keccak256};
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use evm2::{SpecId, bytecode::Bytecode};
use evm2_jit_runtime::runtime::{
    JitBackend, LookupDecision, LookupRequest, RuntimeCacheKey, RuntimeConfig,
};

/// PUSH1 0x42 PUSH0 MSTORE PUSH1 0x20 PUSH0 RETURN.
const BYTECODE_RET42: &[u8] = &[0x60, 0x42, 0x5f, 0x52, 0x60, 0x20, 0x5f, 0xf3];

fn lookup_hit(c: &mut Criterion) {
    let backend = JitBackend::new(RuntimeConfig { enabled: true, ..Default::default() }).unwrap();
    let bytecode = Bytecode::new_legacy(Bytes::from_static(BYTECODE_RET42));
    let key = RuntimeCacheKey { code_hash: keccak256(BYTECODE_RET42), spec_id: SpecId::CANCUN };
    backend.compile_jit_sync(LookupRequest { key, code: bytecode.original_bytes() }).unwrap();
    assert!(backend.get_compiled(key.code_hash, key.spec_id).is_some());

    c.bench_function("lookup/hit", |b| {
        b.iter(|| {
            // As the interpreter runner does for every frame.
            let decision =
                backend.lookup_with(black_box(key), bytecode.original_byte_slice(), || {
                    bytecode.original_bytes()
                });
            assert!(matches!(decision, LookupDecision::Compiled(_)));
            decision
        })
    });
}

criterion_group!(benches, lookup_hit);
criterion_main!(benches);
