#![allow(missing_docs)]

//! Cost of the copy builtins that compiled code calls, over loops of one opcode.

use alloy_primitives::{Address, Bytes};
use criterion::{Criterion, black_box, criterion_group, criterion_main};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId,
    bytecode::Bytecode,
    env::BlockEnvExt,
    ethereum::ethereum_tx_registry,
    evm::{AccountInfo, InMemoryDB},
    interpreter::{InstrStop, Interpreter, MessageExt, op},
};
use evm2_jit_runtime::{
    evm2_evm::run_interpreter,
    runtime::{JitBackend, RuntimeConfig},
};
use std::time::Duration;

/// Loop iterations per frame.
const ITERS: u16 = 1000;

/// The account whose code `EXTCODECOPY` copies.
const OTHER: Address = Address::repeat_byte(0x22);

/// Returns a loop that runs `body` `ITERS` times, then stops.
fn loop_code(body: &[u8]) -> Vec<u8> {
    let [hi, lo] = ITERS.to_be_bytes();
    let mut code = vec![op::PUSH2, hi, lo, op::JUMPDEST];
    code.extend_from_slice(body);
    code.extend([op::PUSH1, 1, op::SWAP1, op::SUB, op::DUP1, op::PUSH1, 3, op::JUMPI, op::STOP]);
    code
}

fn bench_loop(c: &mut Criterion, name: &str, body: &[u8]) {
    let backend =
        JitBackend::new(RuntimeConfig { enabled: true, blocking: true, ..Default::default() })
            .unwrap();
    let mut database = InMemoryDB::default();
    database.insert_account_info(
        &OTHER,
        AccountInfo::default().with_code(Bytecode::new_legacy(Bytes::from(vec![op::JUMPDEST; 64]))),
    );
    let mut host = Evm::<BaseEvmTypes>::new(
        SpecId::CANCUN,
        BlockEnvExt::default(),
        ethereum_tx_registry(SpecId::CANCUN),
        database,
        Precompiles::base(SpecId::CANCUN),
    );
    let message = MessageExt {
        gas_limit: 100_000_000,
        code: Bytecode::new_legacy(Bytes::from(loop_code(body))),
        ..MessageExt::default()
    };
    let mut run = || {
        let mut interpreter = Interpreter::<BaseEvmTypes>::new(&message);
        run_interpreter(&backend, &mut interpreter, &mut host)
    };
    assert_eq!(run(), Some(InstrStop::Stop));

    // Thread CPU time leaves out the time other processes run on this core.
    c.bench_function(name, |b| {
        b.iter_custom(|iters| {
            let start = thread_cpu_time();
            for _ in 0..iters {
                black_box(run());
            }
            thread_cpu_time() - start
        })
    });
}

/// Returns the CPU time this thread has used.
fn thread_cpu_time() -> Duration {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid, writable `timespec`.
    let ret = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    assert_eq!(ret, 0);
    Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}

fn codecopy(c: &mut Criterion) {
    bench_loop(c, "builtins/codecopy", &[op::PUSH1, 32, op::PUSH0, op::PUSH0, op::CODECOPY]);
}

fn extcodecopy(c: &mut Criterion) {
    let mut body = vec![op::PUSH1, 32, op::PUSH0, op::PUSH0, op::PUSH20];
    body.extend_from_slice(OTHER.as_slice());
    body.push(op::EXTCODECOPY);
    bench_loop(c, "builtins/extcodecopy", &body);
}

criterion_group!(benches, codecopy, extcodecopy);
criterion_main!(benches);
