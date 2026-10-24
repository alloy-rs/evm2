//! Run with `LLC_BENCH_CPUS=0,1,2,3,4,5 cargo bench -p evm2-cli --features jit --bench llc`, then
//! again with `LLC_BENCH_CPUS=0,6,12,18,1,7` (one L3 vs spread on an EPYC 9254).
//!
//! Cross-L3 contention on paths every executing thread shares: JIT lookups of resident programs,
//! `Precompiles::base`, and per-frame execution of a contract that calls small leaf contracts,
//! with and without the JIT runtime. Thread `i` is pinned to the `i`-th CPU of
//! `LLC_BENCH_CPUS` on Linux; without it, 4 unpinned threads run.

#![allow(missing_docs)]

use alloy_primitives::{Address, Bytes};
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use evm2::{
    BaseEvmTypes, Evm, Precompiles, SpecId,
    bytecode::Bytecode,
    env::{BlockEnvExt, TxEnvExt},
    ethereum::ethereum_tx_registry,
    evm::{AccountInfo, InMemoryDB},
    interpreter::{Host, InstrStop, MessageExt, op},
};
use evm2_jit_runtime::{
    evm2_evm::JitInterpreterRunner,
    runtime::{
        JitBackend, LookupDecision, LookupRequest, RuntimeCacheKey, RuntimeConfig, RuntimeTuning,
    },
};
use std::{
    hint::black_box,
    sync::Barrier,
    time::{Duration, Instant},
};

const SPEC: SpecId = SpecId::CANCUN;
const CONTRACTS: usize = 4;
const CALLS: usize = 32;

fn llc(c: &mut Criterion) {
    let cpus = cpus();
    let leaves: Vec<_> =
        (0..CONTRACTS).map(|i| Bytecode::new_legacy(Bytes::from(leaf_code(i)))).collect();
    let outer = Bytecode::new_legacy(Bytes::from(outer_code()));
    let backend = backend_with(leaves.iter().chain([&outer]));

    let mut g = c.benchmark_group("llc");
    g.throughput(Throughput::Elements(cpus.len() as u64));
    g.bench_function("jit_lookup", |b| {
        b.iter_custom(|iters| {
            run(
                &cpus,
                iters,
                |_| (),
                |(), t, iters| {
                    for i in 0..iters as usize {
                        let code = &leaves[(i + t) % CONTRACTS];
                        let key = RuntimeCacheKey { code_hash: code.hash_slow(), spec_id: SPEC };
                        let req = LookupRequest { key, code: code.original_bytes() };
                        let LookupDecision::Compiled(program) = backend.lookup(req) else {
                            panic!("program is not resident");
                        };
                        black_box(program);
                    }
                },
            )
        })
    });
    g.bench_function("precompiles_base", |b| {
        b.iter_custom(|iters| {
            run(
                &cpus,
                iters,
                |_| (),
                |(), _, iters| {
                    for _ in 0..iters {
                        black_box(Precompiles::<BaseEvmTypes>::base(SPEC));
                    }
                },
            )
        })
    });

    // One element is one frame: the outer call and each of its leaf calls.
    g.throughput(Throughput::Elements((cpus.len() * (CALLS + 1)) as u64));
    let caller = Address::with_last_byte(0xaa);
    let outer_address = Address::with_last_byte(0xf0);
    let new_evm = |jit: bool| {
        let mut db = InMemoryDB::default();
        db.insert_account_info(&caller, AccountInfo::default());
        db.insert_account_info(&outer_address, AccountInfo::default().with_code(outer.clone()));
        for (i, leaf) in leaves.iter().enumerate() {
            db.insert_account_info(
                &leaf_address(i),
                AccountInfo::default().with_code(leaf.clone()),
            );
        }
        let mut evm = Evm::<BaseEvmTypes>::new(
            SPEC,
            BlockEnvExt::default(),
            ethereum_tx_registry(SPEC),
            db,
            Precompiles::base(SPEC),
        );
        if jit {
            evm.set_interpreter_runner(JitInterpreterRunner::new(backend.clone()));
        }
        evm
    };
    let message = MessageExt {
        gas_limit: 30_000_000,
        destination: outer_address,
        call_target: outer_address,
        code_address: outer_address,
        caller,
        code: outer.clone(),
        ..MessageExt::default()
    };
    let tx_env = TxEnvExt::default();
    for (name, jit) in [("evm_frames/jit", true), ("evm_frames/interpreter", false)] {
        g.bench_function(name, |b| {
            b.iter_custom(|iters| {
                run(
                    &cpus,
                    iters,
                    |_| new_evm(jit),
                    |evm, _, iters| {
                        for _ in 0..iters {
                            let result =
                                Host::execute_message(evm, &tx_env, &mut message.clone()).unwrap();
                            assert_eq!(result.stop, InstrStop::Stop);
                        }
                    },
                )
            })
        });
    }
    g.finish();
}

/// Reads the CPUs to pin threads to from `LLC_BENCH_CPUS`; `None` per thread runs it unpinned.
fn cpus() -> Vec<Option<usize>> {
    match std::env::var("LLC_BENCH_CPUS") {
        Ok(list) => {
            list.split(',').map(|cpu| Some(cpu.trim().parse().expect("bad CPU id"))).collect()
        }
        Err(_) => vec![None; 4],
    }
}

/// Runs `f(state, thread, iters)` on one thread per CPU, started together, after `init(thread)`
/// builds the thread's state there, and returns the slowest thread's time.
fn run<S>(
    cpus: &[Option<usize>],
    iters: u64,
    init: impl Fn(usize) -> S + Sync,
    f: impl Fn(&mut S, usize, u64) + Sync,
) -> Duration {
    let barrier = Barrier::new(cpus.len());
    std::thread::scope(|scope| {
        let handles: Vec<_> = cpus
            .iter()
            .enumerate()
            .map(|(t, &cpu)| {
                let (barrier, init, f) = (&barrier, &init, &f);
                scope.spawn(move || {
                    if let Some(cpu) = cpu {
                        pin(cpu);
                    }
                    let mut state = init(t);
                    // Fill the thread's caches before timing.
                    f(&mut state, t, iters.min(64));
                    barrier.wait();
                    let start = Instant::now();
                    f(&mut state, t, iters);
                    start.elapsed()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).max().unwrap()
    })
}

#[cfg(target_os = "linux")]
fn pin(cpu: usize) {
    // SAFETY: an all-zero `cpu_set_t` is empty, and pid 0 is the calling thread.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(cpu, &mut set);
        let rc = libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &set);
        assert_eq!(rc, 0, "sched_setaffinity({cpu}) failed");
    }
}

#[cfg(not(target_os = "linux"))]
fn pin(_cpu: usize) {}

/// A backend that never compiles on its own, with `codes` compiled and resident.
fn backend_with<'a>(codes: impl IntoIterator<Item = &'a Bytecode>) -> JitBackend {
    let backend = JitBackend::new(RuntimeConfig {
        enabled: true,
        tuning: RuntimeTuning {
            jit_hot_threshold: usize::MAX,
            jit_worker_count: 1,
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();
    for code in codes {
        let key = RuntimeCacheKey { code_hash: code.hash_slow(), spec_id: SPEC };
        backend.compile_jit_sync(LookupRequest { key, code: code.original_bytes() }).unwrap();
        assert!(backend.get_compiled(key.code_hash, SPEC).is_some(), "compile failed");
    }
    backend
}

/// A small contract that returns a word; `i` makes its code hash unique.
fn leaf_code(i: usize) -> Vec<u8> {
    vec![
        op::PUSH2,
        (i >> 8) as u8,
        i as u8,
        op::POP,
        op::PUSH1,
        0x42,
        op::PUSH0,
        op::MSTORE,
        op::PUSH1,
        0x20,
        op::PUSH0,
        op::RETURN,
    ]
}

const fn leaf_address(i: usize) -> Address {
    Address::with_last_byte(0x10 + i as u8)
}

/// A contract that STATICCALLs `CALLS` leaves in turn, then stops.
fn outer_code() -> Vec<u8> {
    let mut code = Vec::new();
    for k in 0..CALLS {
        code.extend([op::PUSH0, op::PUSH0, op::PUSH0, op::PUSH0, op::PUSH20]);
        code.extend_from_slice(leaf_address(k % CONTRACTS).as_slice());
        code.extend([op::GAS, op::STATICCALL, op::POP]);
    }
    code.push(op::STOP);
    code
}

criterion_group!(benches, llc);
criterion_main!(benches);
