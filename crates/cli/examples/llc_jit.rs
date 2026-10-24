//! Measures how JIT compile workers' placement affects block execution on multi-L3 hosts.
//!
//! The main thread replays the mainnet fixture with the interpreter, pinned to one CPU, while
//! compile threads JIT the fixture contracts with LLVM in a loop, pinned to the CPUs given. Compare
//! compile CPUs that share the main thread's last-level cache with CPUs on another one.
//!
//! `cargo run --profile profiling -p evm2-cli --features jit --example llc_jit -- <main_cpu> <compile_cpus|none> <iters>`

use evm2::SpecId;
use evm2_eest::{
    BlockchainTestExecuteConfig, BlockchainTestNoopHook, blockchain_test_pre_state,
    execute_blockchain_test_case_on,
};
use evm2_jit_llvm::EvmLlvmBackend;
use evm2_jit_runtime::EvmCompiler;
use std::{
    hint::black_box,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

fn pin(cpu: usize) {
    // SAFETY: an all-zero cpu_set_t is empty, and pid 0 is the calling thread.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_SET(cpu, &mut set);
        assert_eq!(
            libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &raw const set),
            0,
            "pin to cpu {cpu}"
        );
    }
}

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Collects every contract bytecode in the transaction fixtures.
fn bytecodes() -> Vec<Vec<u8>> {
    fn walk(value: &serde_json::Value, out: &mut Vec<Vec<u8>>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, value) in map {
                    if key == "code"
                        && let Some(code) = value.as_str()
                        && code.len() > 200
                        && let Ok(bytes) = alloy_primitives::hex::decode(code)
                    {
                        out.push(bytes);
                    } else {
                        walk(value, out);
                    }
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(workspace().join("data")).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json")
            && let Ok(text) = std::fs::read_to_string(&path)
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
        {
            walk(&value, &mut out);
        }
    }
    out.sort();
    out.dedup();
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let main_cpu: usize = args[1].parse().unwrap();
    let compile_cpus: Vec<usize> = if args[2] == "none" {
        Vec::new()
    } else {
        args[2].split(',').map(|c| c.parse().unwrap()).collect()
    };
    let iters: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(40);

    pin(main_cpu);
    let path = workspace().join("data/mainnet-25347446-25347455.bin.zst");
    let suite = evm2_eest::read_blockchain_fixture(&path).unwrap();
    let (case_name, case) = suite.0.first_key_value().unwrap();
    let pre_state = blockchain_test_pre_state(&path, case_name, case).unwrap();
    let config = BlockchainTestExecuteConfig { validate_post_state: false, ..Default::default() };

    let codes = Arc::new(bytecodes());
    let stop = Arc::new(AtomicBool::new(false));
    let compiled = Arc::new(AtomicU64::new(0));
    let workers: Vec<_> = compile_cpus
        .iter()
        .enumerate()
        .map(|(worker, &cpu)| {
            let (codes, stop, compiled) = (codes.clone(), stop.clone(), compiled.clone());
            std::thread::spawn(move || {
                pin(cpu);
                let mut next = worker * 7;
                while !stop.load(Ordering::Relaxed) {
                    // A fresh compiler bounds the JIT code each worker keeps alive.
                    let mut compiler = EvmCompiler::new(EvmLlvmBackend::new(false).unwrap());
                    for _ in 0..16 {
                        let code = &codes[next % codes.len()];
                        next += 1;
                        // SAFETY: the compiled function is never called.
                        let _ = unsafe { compiler.jit(&format!("c{next}"), code, SpecId::OSAKA) };
                        let _ = compiler.clear_ir();
                        compiled.fetch_add(1, Ordering::Relaxed);
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                    }
                }
            })
        })
        .collect();

    let mut replay = || {
        let mut db = pre_state.clone();
        let start = Instant::now();
        black_box(
            execute_blockchain_test_case_on(
                &path,
                case_name,
                case,
                &mut db,
                config,
                &mut BlockchainTestNoopHook,
            )
            .unwrap(),
        );
        let elapsed = start.elapsed();
        drop(db);
        elapsed
    };
    for _ in 0..iters / 4 + 1 {
        replay();
    }
    let started = Instant::now();
    let before = compiled.load(Ordering::Relaxed);
    let mut samples: Vec<Duration> = (0..iters).map(|_| replay()).collect();
    let compiles_per_s =
        (compiled.load(Ordering::Relaxed) - before) as f64 / started.elapsed().as_secs_f64();
    stop.store(true, Ordering::Relaxed);
    workers.into_iter().for_each(|w| w.join().unwrap());

    samples.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1e3;
    println!(
        "workload=evm2-jit contracts={} main_cpu={main_cpu} compile_cpus={} p50_ms={:.3} p90_ms={:.3} compiles_per_s={compiles_per_s:.1}",
        codes.len(),
        args[2],
        ms(samples[samples.len() / 2]),
        ms(samples[samples.len() * 9 / 10]),
    );
}
