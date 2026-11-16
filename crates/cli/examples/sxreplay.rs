//! Times block execution of captured mainnet chunks, with and without superinstructions.
//!
//! Usage: sxreplay <reps> <modes> <fixture.json.zst>...
//!
//! Modes (comma separated): `base-cold`, `base-warm`, `sx-cold`, `sx-warm`. `warm` analyzes the
//! pre-state's code before the timer starts, as a node with a code cache has it; `cold` analyzes
//! on first use, inside the timing. Each run gets a fresh pre-state, built and dropped outside the
//! timing, and skips sender recovery and post-state checks, as the mainnet criterion bench does.
//! Before timing, every mode replays each chunk once comparing receipt roots.
//!
//! Prints one tab-separated line per timed run: chunk, mode, rep, nanoseconds.

use evm2::SpecId;
use evm2_eest::{
    BlockchainTestExecuteConfig, BlockchainTestNoopHook, blockchain_test_pre_state,
    execute_blockchain_test_case_on, read_blockchain_fixture,
};
use std::{path::PathBuf, time::Instant};

#[global_allocator]
static ALLOC: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Clone, Copy, PartialEq)]
struct Mode {
    sx: bool,
    warm: bool,
}

fn set_mode(mode: Mode) {
    #[cfg(feature = "sx")]
    evm2_eest::sx::set_enabled(mode.sx);
    #[cfg(not(feature = "sx"))]
    assert!(!mode.sx, "built without superinstructions");
}

fn prewarm(db: &evm2::evm::InMemoryDB, spec: SpecId) {
    #[cfg(feature = "sx")]
    evm2_eest::sx::prewarm(db, spec);
    #[cfg(not(feature = "sx"))]
    {
        let _ = (db, spec);
        panic!("warm modes need the sx build, whose prewarm runs plain jump analysis when off");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let reps: usize = args[0].parse().expect("reps");
    let modes: Vec<(String, Mode)> = args[1]
        .split(',')
        .map(|m| {
            let mode = match m {
                "base-cold" => Mode { sx: false, warm: false },
                "base-warm" => Mode { sx: false, warm: true },
                "sx-cold" => Mode { sx: true, warm: false },
                "sx-warm" => Mode { sx: true, warm: true },
                _ => panic!("unknown mode {m}"),
            };
            (m.to_string(), mode)
        })
        .collect();
    let timed = BlockchainTestExecuteConfig { validate: false, ..Default::default() };
    let checked = BlockchainTestExecuteConfig {
        validate: false,
        compare_receipt_root: true,
        ..Default::default()
    };
    for path in &args[2..] {
        let path = PathBuf::from(path);
        let chunk =
            path.file_name().unwrap().to_string_lossy().trim_end_matches(".json.zst").to_string();
        let suite = read_blockchain_fixture(&path).expect("fixture");
        let (name, case) = suite.0.first_key_value().expect("case");
        let spec = case.network.to_spec_id().expect("single-fork fixture");
        let mut ok = true;
        for (m, mode) in &modes {
            set_mode(*mode);
            let mut db = blockchain_test_pre_state(&path, name, case).expect("pre-state");
            if mode.warm {
                prewarm(&db, spec);
            }
            if let Err(err) = execute_blockchain_test_case_on(
                &path,
                name,
                case,
                &mut db,
                checked,
                &mut BlockchainTestNoopHook,
            ) {
                eprintln!("{chunk} {m}: {err}");
                ok = false;
            }
        }
        if !ok {
            println!("{chunk}\tFAIL\t0\t0");
            continue;
        }
        for rep in 0..reps {
            for k in 0..modes.len() {
                let (m, mode) = &modes[(k + rep) % modes.len()];
                set_mode(*mode);
                let mut db = blockchain_test_pre_state(&path, name, case).expect("pre-state");
                if mode.warm {
                    prewarm(&db, spec);
                }
                let start = Instant::now();
                let result = execute_blockchain_test_case_on(
                    &path,
                    name,
                    case,
                    &mut db,
                    timed,
                    &mut BlockchainTestNoopHook,
                );
                let ns = start.elapsed().as_nanos();
                std::hint::black_box(result).expect("replay");
                println!("{chunk}\t{m}\t{rep}\t{ns}");
                drop(db);
            }
        }
    }
}
