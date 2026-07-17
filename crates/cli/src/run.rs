use alloy_consensus::{TxLegacy, transaction::Recovered};
use alloy_primitives::{Address, B256, Bytes, TxKind, U256, hex, keccak256};
use clap::ValueEnum;
use evm2::{
    BaseEvmTypes, Evm, ExecutionConfig, InterpreterRunner, Precompiles, SpecId, Version,
    bytecode::Bytecode,
    env::BlockEnv,
    ethereum::{RecoveredTxEnvelope, ethereum_tx_registry},
    evm::{AccountInfo, InMemoryDB},
    interpreter::{InstrStop, Interpreter},
};
use evm2_cli::evm_bench;
use evm2_eest::{StateTestPost, StateTestSuite, StateTestUnit};
use evm2_jit_context::EvmCompilerFn;
use evm2_jit_llvm::EvmLlvmBackend;
use evm2_jit_runtime::{
    EvmCompiler, Linker, OptimizationLevel, eyre, parse_asm, shared_library_path,
};
use std::{
    borrow::Cow,
    collections::HashMap,
    hint::black_box,
    path::{Path, PathBuf},
    sync::Arc,
};

const BENCH_CALLER: Address = Address::new([0x11; 20]);
const BENCH_TARGET: Address = Address::new([0xcc; 20]);

#[derive(Debug, clap::Args)]
pub(crate) struct RunArgs {
    /// Benchmark name, path to a file or dump dir, raw hex bytecode, or EVM assembly.
    bench_name: Option<String>,
    #[arg(default_value = "1")]
    n_iters: u64,

    /// List available benchmark names and exit.
    #[arg(long)]
    list: bool,

    #[arg(long)]
    calldata: Option<String>,

    /// Load a shared object file instead of JIT compiling.
    ///
    /// Use with `--aot` to also run the compiled library.
    #[arg(long, num_args = 0..=1, require_equals = true)]
    load: Option<Option<PathBuf>>,

    /// Parse the bytecode only.
    #[arg(long)]
    parse_only: bool,

    /// Print the parsed bytecode IR.
    #[arg(long)]
    display: bool,

    /// Parse the bytecode and render the CFG as a DOT graph.
    #[arg(long, default_missing_value = "svg", num_args = 0..=1)]
    dot: Option<DotFormat>,

    /// Don't open URLs in the browser.
    #[arg(long)]
    no_open: bool,

    /// Compile and link to a shared library.
    #[arg(long)]
    aot: bool,

    /// Interpret the code instead of compiling.
    #[arg(long, conflicts_with = "aot")]
    interpret: bool,

    /// Run JIT only.
    #[arg(long, conflicts_with = "interpret")]
    jit_only: bool,

    /// Compile only, do not link.
    #[arg(long, requires = "aot")]
    no_link: bool,

    #[arg(short = 'o', long)]
    out_dir: Option<PathBuf>,
    #[arg(short = 'O', long, default_value = "2")]
    opt_level: OptimizationLevel,
    #[arg(long, value_enum, default_value = "osaka")]
    spec_id: SpecIdValueEnum,
    #[arg(long)]
    debug_assertions: bool,
    #[arg(long)]
    no_gas: bool,
    #[arg(long)]
    no_len_checks: bool,
    /// Preserve distinct failure results instead of yielding a single `OutOfGas`.
    #[arg(long)]
    no_single_error: bool,
    /// Inspect the stack after the function has been executed.
    #[arg(long)]
    inspect_stack: bool,
    /// Disable frame pointers in the compiled function.
    #[arg(long)]
    no_frame_pointers: bool,
    #[arg(long, default_value = "1000000000")]
    gas_limit: u64,
}

impl RunArgs {
    pub(crate) fn run(self) -> eyre::Result<()> {
        if self.list {
            for bench in get_benches() {
                println!("{}", bench.name);
            }
            return Ok(());
        }

        let Some(bench_name) = self.bench_name.clone() else {
            eyre::bail!("missing <BENCH_NAME>; use `--list` to see available benchmarks");
        };
        let spec_id = self.spec_id.into();
        let calldata = self
            .calldata
            .as_deref()
            .map(|calldata| read_code_string(calldata.trim().as_bytes(), Some("hex")))
            .transpose()?;

        let bench_entry = if let Some(mut bench) =
            get_benches().into_iter().find(|bench| bench.name == bench_name)
        {
            bench.calldata = calldata;
            bench.gas_limit = self.gas_limit;
            bench
        } else if Path::new(&bench_name).exists() {
            let path = Path::new(&bench_name);
            let (name, bytecode_path) = if path.is_dir() {
                let bin = path.join("bytecode.bin");
                eyre::ensure!(bin.is_file(), "{} not found in directory", bin.display());
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| eyre::eyre!("invalid directory name: {}", path.display()))?
                    .to_owned();
                (name, bin)
            } else {
                let name = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| eyre::eyre!("invalid file name: {}", path.display()))?
                    .to_owned();
                (name, path.to_path_buf())
            };
            fixture_from_bytecode(
                name,
                read_code_path(&bytecode_path)?,
                spec_id,
                calldata,
                self.gas_limit,
            )
        } else {
            let bytecode = read_code_string(bench_name.trim().as_bytes(), None).map_err(|err| {
                eyre::eyre!(
                    "{bench_name:?} is not a known benchmark, an existing path, \
                     or valid EVM hex/asm: {err}"
                )
            })?;
            fixture_from_bytecode("custom", bytecode, spec_id, calldata, self.gas_limit)
        };

        let name = bench_entry.name.as_ref();
        let default_aot_dir = || std::env::temp_dir().join("evm2-cli").join(&bench_name);
        let mut aot_dir = None;
        let mut pending_jit = None;

        {
            let bytecode = bench_entry.entry_bytecode()?;
            let compile_spec_id = bench_entry.spec_id;

            let mut compiler = EvmCompiler::new_llvm(self.aot)?;
            compiler.set_opt_level(self.opt_level);
            let out_dir = if self.out_dir.is_some() {
                self.out_dir.clone()
            } else if self.dot.is_some() || self.display || self.parse_only {
                Some(std::env::temp_dir().join("evm2-cli"))
            } else {
                None
            };
            compiler.set_dump_to(out_dir);
            compiler.set_gas_metering(!self.no_gas);
            unsafe { compiler.set_stack_bound_checks(!self.no_len_checks) };
            compiler.set_debug_assertions(self.debug_assertions);
            compiler.set_single_error(!self.no_single_error);

            compiler.set_module_name(name);
            if let Some(dump_dir) = compiler.dump_dir() {
                eprintln!("Dump directory: {}", dump_dir.display());
            }

            compiler.set_inspect_stack(self.inspect_stack);
            if self.no_frame_pointers {
                compiler.set_frame_pointers(false);
            }

            let parsed = compiler.parse(bytecode.as_slice().into(), compile_spec_id)?;
            if self.display || self.parse_only {
                println!("{name}()\n{parsed:#}");
            }
            if let Some(fmt) = self.dot {
                let dump_dir =
                    compiler.dump_dir().expect("dump_dir should be set when --dot is used");
                open_dot(&dump_dir.join("bytecode.dot"), fmt, !self.no_open)?;
            }
            if self.parse_only {
                return Ok(());
            }

            let f_id = compiler.translate_inner(name, &parsed)?;

            if self.aot {
                let out_dir = if let Some(out_dir) = compiler.out_dir() {
                    out_dir.join(&bench_name)
                } else {
                    let dir = default_aot_dir();
                    std::fs::create_dir_all(&dir)?;
                    dir
                };

                let obj = out_dir.join("a.o");
                compiler.write_object_to_file(&obj)?;
                eyre::ensure!(obj.exists(), "failed to write object file");
                eprintln!("Compiled object file to {}", obj.display());

                if !self.no_link {
                    let shared_lib = shared_library_path(&out_dir, "a");
                    let linker = Linker::new();
                    linker.link(&shared_lib, [obj.as_os_str()])?;
                    eyre::ensure!(shared_lib.exists(), "failed to link object file");
                    eprintln!("Linked shared object file to {}", shared_lib.display());
                }

                aot_dir = Some(out_dir);
                if self.load.is_none() {
                    return Ok(());
                }
            } else if self.load.is_none() {
                let func = unsafe { compiler.jit_function(f_id)? };
                let mut functions = HashMap::new();
                functions.insert(keccak256(&bytecode), func);
                compiler.clear_ir()?;
                pending_jit = Some((compiler, functions));
            }
        }

        let load_path = self.load.as_ref().map(|load| match load {
            Some(path) => path.clone(),
            None => {
                let out_dir = aot_dir.unwrap_or_else(default_aot_dir);
                shared_library_path(&out_dir, "a")
            }
        });
        let (prepared, _compiler, _lib);
        if let Some(ref load_path) = load_path {
            let lib;
            (prepared, lib) = PreparedBench::load_from_library(&bench_entry, load_path, name)?;
            _compiler = None;
            _lib = Some(lib);
        } else if let Some((mut compiler, functions)) = pending_jit {
            prepared = PreparedBench::load_with_functions(&bench_entry, &mut compiler, functions)?;
            _compiler = Some(compiler);
            _lib = None;
        } else {
            let compiler;
            (prepared, compiler) = PreparedBench::load(&bench_entry)?;
            _compiler = Some(compiler);
            _lib = None;
        };

        match self.n_iters {
            0 => {}
            1 if !self.interpret || self.jit_only => prepared.sanity_check()?,
            _ => {
                if self.interpret || !self.jit_only {
                    bench(self.n_iters, &format!("{name}/interpreter"), || {
                        prepared.run_interpreter()
                    });
                }
                if !self.interpret {
                    bench(self.n_iters, &format!("{name}/jit"), || prepared.run_jit());
                }
            }
        }

        Ok(())
    }
}

fn open_dot(dot_path: &Path, fmt: DotFormat, open: bool) -> eyre::Result<()> {
    let ext = fmt.extension();
    let out_path = dot_path.with_extension(ext);
    match std::process::Command::new("dot")
        .arg(format!("-T{ext}"))
        .arg("-o")
        .arg(&out_path)
        .arg(dot_path)
        .status()
    {
        Ok(status) if status.success() => {
            eprintln!("DOT graph: {}", out_path.display());
            if open {
                let _ = open::that(out_path.as_os_str());
            }
            return Ok(());
        }
        Ok(status) => eprintln!("warning: dot command failed with {status}, falling back to HTML"),
        Err(err) => eprintln!("warning: dot command not found ({err}), falling back to HTML"),
    }

    let dot_source = std::fs::read_to_string(dot_path)?;
    let dot_escaped = dot_source.replace('\\', "\\\\").replace('`', "\\`").replace("${", "\\${");
    let html_path = dot_path.with_extension("html");
    std::fs::write(
        &html_path,
        format!(
            r#"<!DOCTYPE html>
<html><head>
<meta charset="utf-8">
<title>evm2 CFG</title>
<style>
html,body{{margin:0;height:100%;overflow:hidden;background:#1a1a2e}}
#graph{{width:100%;height:100%}}
</style>
</head><body>
<div id="graph"></div>
<script type="module">
import {{ instance }} from "https://cdn.jsdelivr.net/npm/@viz-js/viz@3/+esm";
import svgPanZoom from "https://cdn.jsdelivr.net/npm/svg-pan-zoom@3/+esm";
const viz = await instance();
const svg = viz.renderSVGElement(`{dot_escaped}`);
svg.setAttribute("width", "100%");
svg.setAttribute("height", "100%");
document.getElementById("graph").appendChild(svg);
svgPanZoom(svg, {{zoomScaleSensitivity:0.3, minZoom:0.1, maxZoom:50, controlIconsEnabled:true}});
</script>
</body></html>"#
        ),
    )?;
    eprintln!("DOT graph: {}", html_path.display());
    if open {
        let _ = open::that(html_path.as_os_str());
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum DotFormat {
    Svg,
    Png,
}

impl DotFormat {
    const fn extension(self) -> &'static str {
        match self {
            Self::Svg => "svg",
            Self::Png => "png",
        }
    }
}

fn bench<T>(n_iters: u64, name: &str, mut f: impl FnMut() -> eyre::Result<T>) {
    let warmup = (n_iters / 10).max(10);
    for _ in 0..warmup {
        black_box(f().expect("warmup execution failed"));
    }

    let t = std::time::Instant::now();
    for _ in 0..n_iters {
        black_box(f().expect("benchmark execution failed"));
    }
    let d = t.elapsed();
    eprintln!("{name}: {:>9?} ({d:>12?} / {n_iters})", d / n_iters as u32);
}

#[derive(Clone, Copy, Debug, ValueEnum)]
#[clap(rename_all = "lowercase")]
#[allow(non_camel_case_types, clippy::upper_case_acronyms)]
enum SpecIdValueEnum {
    FRONTIER,
    FRONTIER_THAWING,
    HOMESTEAD,
    DAO_FORK,
    TANGERINE,
    SPURIOUS_DRAGON,
    BYZANTIUM,
    CONSTANTINOPLE,
    PETERSBURG,
    ISTANBUL,
    MUIR_GLACIER,
    BERLIN,
    LONDON,
    ARROW_GLACIER,
    GRAY_GLACIER,
    MERGE,
    SHANGHAI,
    CANCUN,
    PRAGUE,
    OSAKA,
    AMSTERDAM,
    LATEST,
}

impl From<SpecIdValueEnum> for SpecId {
    fn from(value: SpecIdValueEnum) -> Self {
        match value {
            SpecIdValueEnum::FRONTIER => Self::FRONTIER,
            SpecIdValueEnum::FRONTIER_THAWING => Self::FRONTIER,
            SpecIdValueEnum::HOMESTEAD => Self::HOMESTEAD,
            SpecIdValueEnum::DAO_FORK => Self::HOMESTEAD,
            SpecIdValueEnum::TANGERINE => Self::TANGERINE,
            SpecIdValueEnum::SPURIOUS_DRAGON => Self::SPURIOUS_DRAGON,
            SpecIdValueEnum::BYZANTIUM => Self::BYZANTIUM,
            SpecIdValueEnum::CONSTANTINOPLE => Self::PETERSBURG,
            SpecIdValueEnum::PETERSBURG => Self::PETERSBURG,
            SpecIdValueEnum::ISTANBUL => Self::ISTANBUL,
            SpecIdValueEnum::MUIR_GLACIER => Self::ISTANBUL,
            SpecIdValueEnum::BERLIN => Self::BERLIN,
            SpecIdValueEnum::LONDON => Self::LONDON,
            SpecIdValueEnum::ARROW_GLACIER => Self::LONDON,
            SpecIdValueEnum::GRAY_GLACIER => Self::LONDON,
            SpecIdValueEnum::MERGE => Self::MERGE,
            SpecIdValueEnum::SHANGHAI => Self::SHANGHAI,
            SpecIdValueEnum::CANCUN => Self::CANCUN,
            SpecIdValueEnum::PRAGUE => Self::PRAGUE,
            SpecIdValueEnum::OSAKA => Self::OSAKA,
            SpecIdValueEnum::AMSTERDAM => Self::AMSTERDAM,
            SpecIdValueEnum::LATEST => Self::OSAKA,
        }
    }
}

#[derive(Clone, Debug)]
struct Bench {
    name: Cow<'static, str>,
    source: BenchSource,
    spec_id: SpecId,
    calldata: Option<Vec<u8>>,
    gas_limit: u64,
    assert_success: bool,
}

#[derive(Clone, Debug)]
enum BenchSource {
    Fixture(&'static str),
    Bytecode(Vec<u8>),
}

impl Bench {
    const fn from_catalog(bench: &evm_bench::Bench) -> Option<Self> {
        let evm_bench::BenchKind::Transaction { spec } = bench.kind else {
            return None;
        };
        Some(Self {
            name: Cow::Borrowed(bench.name),
            source: BenchSource::Fixture(bench.fixture_path),
            spec_id: spec,
            calldata: None,
            gas_limit: u64::MAX,
            assert_success: true,
        })
    }

    fn entry_bytecode(&self) -> eyre::Result<Vec<u8>> {
        match &self.source {
            BenchSource::Fixture(fixture_path) => {
                let fixture_json = read_workspace_text(fixture_path)?;
                let suite = parse_suite(&fixture_json)?;
                let (_, unit, _) = select_case(&suite, &self.name, self.spec_id)?;
                let Some(target) = unit.transaction.to else {
                    eyre::bail!("fixture transaction creates a contract");
                };
                let account = unit
                    .pre
                    .get(&target)
                    .ok_or_else(|| eyre::eyre!("fixture missing entry-point account {target}"))?;
                Ok(account.code.to_vec())
            }
            BenchSource::Bytecode(bytecode) => Ok(bytecode.clone()),
        }
    }
}

fn fixture_from_bytecode(
    name: impl Into<Cow<'static, str>>,
    bytecode: Vec<u8>,
    spec_id: SpecId,
    calldata: Option<Vec<u8>>,
    gas_limit: u64,
) -> Bench {
    Bench {
        name: name.into(),
        source: BenchSource::Bytecode(bytecode),
        spec_id,
        calldata,
        gas_limit,
        assert_success: false,
    }
}

fn get_benches() -> Vec<Bench> {
    evm_bench::BENCHES.iter().filter_map(Bench::from_catalog).collect()
}

#[derive(Clone, Debug)]
struct PreparedBench {
    name: Cow<'static, str>,
    spec_id: SpecId,
    block: BlockEnv,
    db: InMemoryDB,
    tx: RecoveredTxEnvelope,
    functions: Arc<HashMap<B256, EvmCompilerFn>>,
    assert_success: bool,
}

type LlvmCompiler = EvmCompiler<EvmLlvmBackend>;

impl PreparedBench {
    fn load(bench: &Bench) -> eyre::Result<(Self, LlvmCompiler)> {
        let mut compiler = EvmCompiler::new_llvm(false)?;
        let prepared = Self::load_with_functions(bench, &mut compiler, HashMap::new())?;
        Ok((prepared, compiler))
    }

    fn load_with_functions(
        bench: &Bench,
        compiler: &mut LlvmCompiler,
        mut functions: HashMap<B256, EvmCompilerFn>,
    ) -> eyre::Result<Self> {
        let (name, accounts, block, db, tx) = parse_bench(bench)?;
        for account in &accounts {
            if account.bytecode.is_empty() || functions.contains_key(&account.code_hash) {
                continue;
            }
            let name = format!("contract_{}", hex::encode(account.code_hash));
            let func = unsafe {
                compiler.jit(&name, account.bytecode.original_byte_slice(), bench.spec_id)?
            };
            functions.insert(account.code_hash, func);
            compiler.clear_ir()?;
        }

        Ok(Self {
            name,
            spec_id: bench.spec_id,
            block,
            db,
            tx,
            functions: Arc::new(functions),
            assert_success: bench.assert_success,
        })
    }

    fn load_from_library(
        bench: &Bench,
        lib_path: &Path,
        symbol_name: &str,
    ) -> eyre::Result<(Self, libloading::Library)> {
        let (name, accounts, block, db, tx) = parse_bench(bench)?;
        let lib = unsafe { libloading::Library::new(lib_path) }?;
        let mut functions = HashMap::new();
        for account in &accounts {
            if account.bytecode.is_empty() || functions.contains_key(&account.code_hash) {
                continue;
            }
            let func = unsafe {
                let sym: libloading::Symbol<'_, EvmCompilerFn> = lib.get(symbol_name.as_bytes())?;
                *sym
            };
            functions.insert(account.code_hash, func);
        }

        Ok((
            Self {
                name,
                spec_id: bench.spec_id,
                block,
                db,
                tx,
                functions: Arc::new(functions),
                assert_success: bench.assert_success,
            },
            lib,
        ))
    }

    fn run_interpreter(&self) -> eyre::Result<evm2::TxResult> {
        let mut evm = self.new_evm();
        evm.transact(&self.tx).map(evm2::ExecutedTx::commit).map_err(Into::into)
    }

    fn run_jit(&self) -> eyre::Result<evm2::TxResult> {
        let mut evm = self.new_evm();
        evm.set_interpreter_runner(FixedJitRunner { functions: Arc::clone(&self.functions) });
        evm.transact(&self.tx).map(evm2::ExecutedTx::commit).map_err(Into::into)
    }

    fn sanity_check(&self) -> eyre::Result<()> {
        let interpreter = self.run_interpreter()?;
        let jit = self.run_jit()?;
        if self.assert_success {
            eyre::ensure!(
                interpreter.status,
                "{} benchmark transaction failed:\n  interpreter: {:?}\n  JIT: {:?}",
                self.name,
                interpreter,
                jit
            );
        }
        eyre::ensure!(
            interpreter.status == jit.status,
            "interpreter and JIT results differ:\n  interpreter: {:?}\n  JIT: {:?}",
            interpreter,
            jit
        );
        Ok(())
    }

    fn new_evm(&self) -> Evm<'static, BaseEvmTypes> {
        Evm::new(
            self.spec_id,
            self.block,
            ethereum_tx_registry(self.spec_id),
            self.db.clone(),
            Precompiles::base(self.spec_id),
        )
    }
}

#[derive(Clone, Debug)]
struct FixedJitRunner {
    functions: Arc<HashMap<B256, EvmCompilerFn>>,
}

impl InterpreterRunner<BaseEvmTypes> for FixedJitRunner {
    fn run<'frame, 'host>(
        &self,
        config: &ExecutionConfig<BaseEvmTypes>,
        interpreter: &mut Interpreter<'frame, 'host, BaseEvmTypes>,
        host: &mut Evm<'host, BaseEvmTypes>,
    ) -> Option<InstrStop> {
        let code = interpreter.original_bytecode();
        let func = *self.functions.get(&keccak256(&code))?;
        interpreter.prepare_run(config.base_spec_id(), config.version(), host);
        Some(unsafe { func.call_with_interpreter(interpreter) })
    }
}

#[derive(Clone, Debug)]
struct ParsedAccount {
    bytecode: Bytecode,
    code_hash: B256,
}

type PreparedBenchParts =
    (Cow<'static, str>, Vec<ParsedAccount>, BlockEnv, InMemoryDB, RecoveredTxEnvelope);

fn parse_bench(bench: &Bench) -> eyre::Result<PreparedBenchParts> {
    match &bench.source {
        BenchSource::Fixture(fixture_path) => parse_fixture_bench(bench, fixture_path),
        BenchSource::Bytecode(bytecode) => parse_bytecode_bench(bench, bytecode),
    }
}

fn parse_fixture_bench(bench: &Bench, fixture_path: &str) -> eyre::Result<PreparedBenchParts> {
    let fixture_json = read_workspace_text(fixture_path)?;
    let suite = parse_suite(&fixture_json)?;
    let (case_name, unit, post) = select_case(&suite, &bench.name, bench.spec_id)?;
    let block = parse_block(&unit.env);
    let mut db = InMemoryDB::default();
    let mut accounts = Vec::new();
    for (address, account) in &unit.pre {
        let bytecode = Bytecode::new_legacy(account.code.clone());
        let code_hash = bytecode.hash_slow();
        let info = AccountInfo::default()
            .with_balance(account.balance)
            .with_nonce(account.nonce)
            .with_code(bytecode.clone());
        db.insert_account_info(address, info);
        for (key, value) in &account.storage {
            db.insert_account_storage(address, key, value);
        }
        accounts.push(ParsedAccount { bytecode, code_hash });
    }
    let tx = build_fixture_tx(unit, post, bench)?;
    Ok((Cow::Owned(case_name.to_owned()), accounts, block, db, tx))
}

fn parse_bytecode_bench(bench: &Bench, bytecode: &[u8]) -> eyre::Result<PreparedBenchParts> {
    let mut db = InMemoryDB::default();
    db.insert_account_info(
        &BENCH_CALLER,
        AccountInfo::default().with_balance(U256::from_limbs([u64::MAX, u64::MAX, u64::MAX, 0])),
    );
    let bytecode = Bytecode::new_legacy(Bytes::copy_from_slice(bytecode));
    let code_hash = bytecode.hash_slow();
    db.insert_account_info(&BENCH_TARGET, AccountInfo::default().with_code(bytecode.clone()));
    let gas_limit = bench.gas_limit();
    let tx = TxLegacy {
        chain_id: None,
        nonce: 0,
        gas_price: 0,
        gas_limit,
        to: TxKind::Call(BENCH_TARGET),
        value: U256::ZERO,
        input: bench.calldata.clone().map(Bytes::from).unwrap_or_default(),
    };
    Ok((
        bench.name.clone(),
        vec![ParsedAccount { bytecode, code_hash }],
        BlockEnv::default(),
        db,
        RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(tx, BENCH_CALLER)),
    ))
}

fn parse_suite(fixture_json: &str) -> eyre::Result<StateTestSuite> {
    serde_json::from_str(fixture_json).map_err(Into::into)
}

fn select_case<'a>(
    suite: &'a StateTestSuite,
    name: &str,
    spec_id: SpecId,
) -> eyre::Result<(&'a str, &'a StateTestUnit, &'a StateTestPost)> {
    let (case_name, unit) = if let Some((case_name, unit)) = suite.0.get_key_value(name) {
        (case_name.as_str(), unit)
    } else if suite.0.len() == 1 {
        let (case_name, unit) =
            suite.0.iter().next().expect("single-entry suite must contain a case");
        (case_name.as_str(), unit)
    } else {
        eyre::bail!("fixture suite does not contain benchmark case {name}");
    };
    let post = unit
        .post
        .iter()
        .filter(|(spec_name, _)| spec_name.to_spec_id() == Some(spec_id))
        .flat_map(|(_, posts)| posts)
        .next()
        .ok_or_else(|| {
            eyre::eyre!("fixture suite does not contain {case_name} post for {spec_id:?}")
        })?;
    Ok((case_name, unit, post))
}

fn parse_block(env: &evm2_eest::StateTestEnv) -> BlockEnv {
    BlockEnv {
        number: env.current_number,
        beneficiary: env.current_coinbase,
        timestamp: env.current_timestamp,
        gas_limit: env.current_gas_limit,
        basefee: env.current_base_fee.unwrap_or_default(),
        difficulty: env.current_difficulty,
        prevrandao: env.current_random.map_or(U256::ZERO, |value| U256::from_be_bytes(value.0)),
        slot_num: env.slot_number.unwrap_or_default(),
        ..BlockEnv::default()
    }
}

fn build_fixture_tx(
    unit: &StateTestUnit,
    post: &StateTestPost,
    bench: &Bench,
) -> eyre::Result<RecoveredTxEnvelope> {
    let raw = &unit.transaction;
    let indexes = post.indexes;
    let data = if let Some(calldata) = &bench.calldata {
        Bytes::copy_from_slice(calldata)
    } else {
        raw.data
            .get(indexes.data)
            .cloned()
            .ok_or_else(|| eyre::eyre!("fixture data index {} does not exist", indexes.data))?
    };
    let gas_limit = *raw
        .gas_limit
        .get(indexes.gas)
        .ok_or_else(|| eyre::eyre!("fixture gas index {} does not exist", indexes.gas))?;
    let gas_limit = u64_value(gas_limit)?.min(bench.gas_limit());
    let value = *raw
        .value
        .get(indexes.value)
        .ok_or_else(|| eyre::eyre!("fixture value index {} does not exist", indexes.value))?;
    let caller =
        raw.sender.ok_or_else(|| eyre::eyre!("benchmark transaction sender is required"))?;
    let tx = TxLegacy {
        chain_id: None,
        nonce: u64_value(raw.nonce)?,
        gas_price: u128_value(raw.gas_price.unwrap_or_default())?,
        gas_limit,
        to: raw.to.map_or(TxKind::Create, TxKind::Call),
        value,
        input: data,
    };
    Ok(RecoveredTxEnvelope::Legacy(Recovered::new_unchecked(tx, caller)))
}

impl Bench {
    fn gas_limit(&self) -> u64 {
        self.gas_limit.min(Version::base(self.spec_id).tx_gas_limit_cap)
    }
}

fn u128_value(value: U256) -> eyre::Result<u128> {
    value.try_into().map_err(|_| eyre::eyre!("fixture value overflows u128"))
}

fn u64_value(value: U256) -> eyre::Result<u64> {
    value.try_into().map_err(|_| eyre::eyre!("fixture value overflows u64"))
}

fn read_code_path(path: &Path) -> eyre::Result<Vec<u8>> {
    let contents = std::fs::read(path)?;
    let ext = path.extension().and_then(|s| s.to_str());
    read_code_string(&contents, ext)
}

fn read_workspace_text(path: &str) -> eyre::Result<String> {
    let path = workspace_path(path);
    std::fs::read_to_string(&path)
        .map_err(|err| eyre::eyre!("failed to read {}: {err}", path.display()))
}

fn workspace_path(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(path)
}

fn read_code_string(contents: &[u8], ext: Option<&str>) -> eyre::Result<Vec<u8>> {
    let has_prefix = contents.starts_with(b"0x") || contents.starts_with(b"0X");
    let is_hex = ext != Some("bin") && (ext == Some("hex") || has_prefix);
    let utf8 = || {
        std::str::from_utf8(contents)
            .map(str::trim)
            .map_err(|err| eyre::eyre!("given code is not valid UTF-8: {err}"))
    };
    if is_hex {
        let input = utf8()?;
        let mut lines = input.lines().map(str::trim);
        let first_line = lines.next().unwrap_or_default();
        hex::decode(first_line).map_err(|err| eyre::eyre!("given code is not valid hex: {err}"))
    } else if ext == Some("bin") || !contents.is_ascii() {
        Ok(contents.to_vec())
    } else if ext == Some("evm") {
        parse_asm(utf8()?)
    } else if contents.is_ascii() {
        let s = utf8()?;
        parse_asm(s).or_else(|err1| match hex::decode(s) {
            Ok(bytes) => Ok(bytes),
            Err(err2) => {
                Err(eyre::eyre!("input is not valid EVM bytecode or hex:\n1. {err1}\n2. {err2}"))
            }
        })
    } else {
        eyre::bail!("could not determine bytecode type");
    }
}

#[cfg(test)]
mod tests {
    use super::{SpecId, SpecIdValueEnum, read_code_string};
    use crate::args::{Args, Command};
    use clap::Parser;

    #[test]
    fn read_code_string_accepts_prefixed_hex() {
        assert_eq!(read_code_string(b"0x6000", None).unwrap(), [0x60, 0x00]);
    }

    #[test]
    fn latest_spec_matches_default_run_spec() {
        assert_eq!(SpecId::from(SpecIdValueEnum::LATEST), SpecId::OSAKA);
    }

    #[test]
    fn load_without_value_does_not_consume_bench_name() {
        let args = Args::try_parse_from(["evm2", "run", "--aot", "--load", "STOP", "1"]).unwrap();
        let Command::Run(command) = args.command else {
            panic!("expected run command");
        };

        assert_eq!(command.load, Some(None));
        assert_eq!(command.bench_name.as_deref(), Some("STOP"));
        assert_eq!(command.n_iters, 1);
    }
}
