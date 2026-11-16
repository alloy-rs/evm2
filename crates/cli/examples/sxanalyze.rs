//! Times code analysis per byte on real mainnet contracts: jump destination analysis (what main
//! runs), translation to superinstructions (which also marks jump destinations), and LLVM JIT
//! compilation.
//!
//! Usage: sxanalyze <corpus> <jit sample size>
//!
//! `corpus` is a directory of segment profiles (`EVM2_SEG_PROFILE` output, for the contracts a
//! replay executed) or a `codes.bin` of unique runtime codes (u32 len, u32 deploys,
//! u32 first block, bytes; the first 300,000 nonempty ones are used). Each analysis runs on
//! fresh `Bytecode`s built outside the timing, for Osaka.

use alloy_primitives::Bytes;
use evm2::{
    BaseEvmTypes, EvmTypesHost, SpecId, bytecode::Bytecode, evm::config::EvmConfigSelector,
};
use std::time::Instant;

#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn load(path: &str) -> Vec<Vec<u8>> {
    let mut codes = Vec::new();
    if std::path::Path::new(path).is_dir() {
        let mut seen = std::collections::HashSet::new();
        let mut files: Vec<_> =
            std::fs::read_dir(path).unwrap().map(|e| e.unwrap().path()).collect();
        files.sort();
        for f in files {
            let d = std::fs::read(&f).unwrap();
            let n = u64::from_le_bytes(d[16..24].try_into().unwrap()) as usize;
            let mut p = 24;
            for _ in 0..n {
                let l = u32::from_le_bytes(d[p..p + 4].try_into().unwrap()) as usize;
                let c = d[p + 4..p + 4 + l].to_vec();
                if l > 0 && seen.insert(c.clone()) {
                    codes.push(c);
                }
                p += 4 + l;
            }
        }
    } else {
        let d = std::fs::read(path).unwrap();
        let mut p = 0;
        while p + 12 <= d.len() && codes.len() < 300_000 {
            let l = u32::from_le_bytes(d[p..p + 4].try_into().unwrap()) as usize;
            p += 12;
            if l > 0 {
                codes.push(d[p..p + l].to_vec());
            }
            p += l;
        }
    }
    codes
}

fn pct(v: &mut [f64], q: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * q) as usize]
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let codes = load(&args[0]);
    let jit_n: usize = args[1].parse().unwrap();
    let bytes: usize = codes.iter().map(Vec::len).sum();
    println!("corpus\t{}\tcodes\t{}\tbytes\t{}", args[0], codes.len(), bytes);
    let mut config = <<BaseEvmTypes as EvmTypesHost>::ConfigSelector as EvmConfigSelector<
        BaseEvmTypes,
    >>::execution_config(SpecId::OSAKA);

    for round in 0..3 {
        for (name, sx) in [("jumpdest", false), ("sx-translate", true)] {
            config.set_superinstructions(sx);
            let fresh: Vec<Bytecode> =
                codes.iter().map(|c| Bytecode::new_legacy(Bytes::from(c.clone()))).collect();
            let mut per = Vec::with_capacity(codes.len());
            let start = Instant::now();
            for code in &fresh {
                let t = Instant::now();
                config.analyze(code);
                per.push((t.elapsed().as_nanos() as f64, code.original_byte_slice().len()));
            }
            let total = start.elapsed().as_nanos() as f64;
            let mut us: Vec<f64> = per.iter().map(|p| p.0 / 1e3).collect();
            println!(
                "{name}\tround {round}\t{:.3} ns/byte\t{:.0} MB/s\tper contract median {:.1} us p90 {:.1} us p99 {:.1} us",
                total / bytes as f64,
                bytes as f64 / total * 1e3,
                pct(&mut us, 0.5),
                pct(&mut us, 0.9),
                pct(&mut us, 0.99)
            );
            drop(fresh);
        }
    }

    #[cfg(feature = "jit")]
    {
        use evm2_jit_llvm::EvmLlvmBackend;
        use evm2_jit_runtime::{EvmCompiler, OptimizationLevel};
        // An even sample across the corpus.
        let step = (codes.len() / jit_n.max(1)).max(1);
        let sample: Vec<&Vec<u8>> = codes.iter().step_by(step).take(jit_n).collect();
        let sbytes: usize = sample.iter().map(|c| c.len()).sum();
        for (name, level) in
            [("jit-O0", OptimizationLevel::None), ("jit-default", OptimizationLevel::Default)]
        {
            let mut compiler = EvmCompiler::<EvmLlvmBackend>::new_llvm(false).expect("LLVM");
            compiler.set_opt_level(level);
            let mut us = Vec::with_capacity(sample.len());
            let mut failed = 0;
            let start = Instant::now();
            for (i, code) in sample.iter().enumerate() {
                let t = Instant::now();
                // SAFETY: The function is never called.
                if unsafe { compiler.jit(&format!("c{i}"), code, SpecId::OSAKA) }.is_err() {
                    failed += 1;
                }
                compiler.clear_ir().expect("clear IR");
                us.push(t.elapsed().as_nanos() as f64 / 1e3);
            }
            let total = start.elapsed().as_nanos() as f64;
            println!(
                "{name}\t{} codes\t{:.1} ns/byte\t{:.2} MB/s\tper contract median {:.0} us p90 {:.0} us p99 {:.0} us\tfailed {failed}",
                sample.len(),
                total / sbytes as f64,
                sbytes as f64 / total * 1e3,
                pct(&mut us, 0.5),
                pct(&mut us, 0.9),
                pct(&mut us, 0.99)
            );
        }
    }
    #[cfg(not(feature = "jit"))]
    let _ = jit_n;
}
