use evm2::SpecId;
use std::borrow::Cow;

#[derive(Clone, Copy, Debug)]
pub struct Bench {
    pub name: &'static str,
    pub fixture_path: &'static str,
    pub kind: BenchKind,
    /// How much of the JIT pipeline the benchmark exercises.
    pub jit: JitBench,
    /// Whether the `jit-mainnet` CodSpeed shard runs the benchmark instead of the `jit` shard.
    ///
    /// The mainnet contracts take roughly half of the JIT benchmark time, so they run apart from
    /// the rest to keep each shard short.
    pub mainnet: bool,
}

#[derive(Clone, Debug)]
pub struct BenchCase {
    pub name: Cow<'static, str>,
    pub fixture_path: &'static str,
    pub kind: BenchCaseKind,
    pub jit: JitBench,
}

/// How much of the JIT pipeline a benchmark exercises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JitBench {
    /// Benchmark translation, compilation, and execution.
    Full,
    /// Benchmark translation and execution, but not compilation.
    ///
    /// Repeated LLVM compilations of a large contract are too slow for CI under valgrind.
    RunOnly,
    /// Do not benchmark the JIT, because compiling costs far more than the benchmark measures.
    Skip,
}

#[derive(Clone, Copy, Debug)]
pub enum BenchKind {
    Transaction { spec: SpecId },
    TransactionSuite { spec: SpecId },
    BlockchainReplay,
}

#[derive(Clone, Copy, Debug)]
pub enum BenchCaseKind {
    Transaction { spec: SpecId },
    BlockchainReplay,
}

impl Bench {
    pub const fn transaction(name: &'static str, spec: SpecId, fixture_path: &'static str) -> Self {
        Self {
            name,
            fixture_path,
            kind: BenchKind::Transaction { spec },
            jit: JitBench::Full,
            mainnet: false,
        }
    }

    pub const fn transaction_suite(spec: SpecId, fixture_path: &'static str) -> Self {
        Self {
            name: "",
            fixture_path,
            kind: BenchKind::TransactionSuite { spec },
            jit: JitBench::Full,
            mainnet: false,
        }
    }

    pub const fn blockchain_replay(name: &'static str, fixture_path: &'static str) -> Self {
        Self {
            name,
            fixture_path,
            kind: BenchKind::BlockchainReplay,
            jit: JitBench::Skip,
            mainnet: false,
        }
    }

    /// Sets how much of the JIT pipeline the benchmark exercises.
    pub const fn jit(mut self, jit: JitBench) -> Self {
        self.jit = jit;
        self
    }

    /// Moves the benchmark to the `jit-mainnet` shard.
    pub const fn mainnet(mut self) -> Self {
        self.mainnet = true;
        self
    }

    pub const fn transaction_fixture_path(&self) -> Option<&'static str> {
        match self.kind {
            BenchKind::Transaction { .. } | BenchKind::TransactionSuite { .. } => {
                Some(self.fixture_path)
            }
            BenchKind::BlockchainReplay => None,
        }
    }
}

impl BenchCase {
    pub fn transaction(
        name: impl Into<Cow<'static, str>>,
        spec: SpecId,
        fixture_path: &'static str,
        jit: JitBench,
    ) -> Self {
        Self { name: name.into(), fixture_path, kind: BenchCaseKind::Transaction { spec }, jit }
    }

    pub const fn blockchain_replay(name: &'static str, fixture_path: &'static str) -> Self {
        Self {
            name: Cow::Borrowed(name),
            fixture_path,
            kind: BenchCaseKind::BlockchainReplay,
            jit: JitBench::Skip,
        }
    }

    pub const fn transaction_spec(&self) -> Option<SpecId> {
        match self.kind {
            BenchCaseKind::Transaction { spec } => Some(spec),
            BenchCaseKind::BlockchainReplay => None,
        }
    }
}

pub static BENCHES: &[Bench] = &[
    Bench::transaction("fibonacci-calldata", SpecId::OSAKA, "data/fibonacci-calldata.json"),
    Bench::transaction("factorial", SpecId::OSAKA, "data/factorial.json"),
    Bench::transaction("counter", SpecId::OSAKA, "data/counter.json"),
    Bench::transaction("snailtracer", SpecId::CANCUN, "data/snailtracer.json")
        .jit(JitBench::RunOnly),
    Bench::transaction("weth", SpecId::OSAKA, "data/weth.json"),
    Bench::transaction("hash_10k", SpecId::OSAKA, "data/hash_10k.json"),
    Bench::transaction("erc20_transfer", SpecId::OSAKA, "data/erc20_transfer.json"),
    Bench::transaction("push0_proxy", SpecId::OSAKA, "data/push0_proxy.json"),
    Bench::transaction("usdc_proxy", SpecId::OSAKA, "data/usdc_proxy.json").jit(JitBench::RunOnly),
    Bench::transaction("fiat_token", SpecId::OSAKA, "data/fiat_token.json").jit(JitBench::RunOnly),
    Bench::transaction("uniswap_v2_pair", SpecId::OSAKA, "data/uniswap_v2_pair.json")
        .jit(JitBench::RunOnly),
    Bench::transaction("univ2_router", SpecId::OSAKA, "data/univ2_router.json")
        .jit(JitBench::RunOnly),
    // A cheap `getOrderStatus` call into a large contract: compiling takes 0.8 s for a 1.6 us run.
    Bench::transaction("seaport", SpecId::OSAKA, "data/seaport.json").jit(JitBench::Skip),
    Bench::transaction("airdrop", SpecId::OSAKA, "data/airdrop.json").jit(JitBench::RunOnly),
    Bench::transaction("bswap64", SpecId::OSAKA, "data/bswap64.json"),
    Bench::transaction("bswap64_opt", SpecId::OSAKA, "data/bswap64_opt.json"),
    Bench::transaction("eip4788", SpecId::OSAKA, "data/eip4788.json"),
    Bench::transaction("eip2935", SpecId::OSAKA, "data/eip2935.json"),
    Bench::transaction("burntpix", SpecId::CANCUN, "data/burntpix.json"),
    Bench::transaction("curve_stableswap", SpecId::CANCUN, "data/curve-stableswap-2pool.json"),
    Bench::transaction("onchain_lm_v2", SpecId::CANCUN, "data/onchain-lm-v2.json"),
    Bench::transaction("usdt", SpecId::OSAKA, "data/usdt.json").mainnet(),
    Bench::transaction("universal_router", SpecId::OSAKA, "data/universal_router.json")
        .jit(JitBench::RunOnly)
        .mainnet(),
    Bench::transaction("aggregation_router_v6", SpecId::OSAKA, "data/aggregation_router_v6.json")
        .jit(JitBench::RunOnly)
        .mainnet(),
    Bench::transaction("entry_point_v0_7", SpecId::OSAKA, "data/entry_point_v0_7.json").mainnet(),
    Bench::transaction("uniswap_v3_pool", SpecId::OSAKA, "data/uniswap_v3_pool.json")
        .jit(JitBench::RunOnly)
        .mainnet(),
    Bench::transaction(
        "uniswap_v4_pool_manager",
        SpecId::OSAKA,
        "data/uniswap_v4_pool_manager.json",
    )
    .jit(JitBench::RunOnly)
    .mainnet(),
    Bench::transaction("curve_3pool", SpecId::OSAKA, "data/curve_3pool.json")
        .jit(JitBench::RunOnly)
        .mainnet(),
    Bench::transaction("curve_tricrypto2", SpecId::OSAKA, "data/curve_tricrypto2.json")
        .jit(JitBench::RunOnly)
        .mainnet(),
    Bench::transaction("balancer_vault", SpecId::OSAKA, "data/balancer_vault.json")
        .jit(JitBench::RunOnly)
        .mainnet(),
    Bench::transaction("succinct_staking", SpecId::OSAKA, "data/succinct_staking.json")
        .jit(JitBench::RunOnly)
        .mainnet(),
    Bench::transaction_suite(SpecId::ISTANBUL, "data/precompile-blake2.json"),
    Bench::transaction_suite(SpecId::ISTANBUL, "data/precompile-bn254.json"),
    Bench::transaction_suite(SpecId::PRAGUE, "data/precompile-bls12-381.json"),
    Bench::transaction_suite(SpecId::ISTANBUL, "data/precompile-ecrecover.json"),
    Bench::transaction_suite(SpecId::OSAKA, "data/precompile-identity.json"),
    Bench::transaction_suite(SpecId::CANCUN, "data/precompile-kzg-point-evaluation.json"),
    Bench::transaction_suite(SpecId::BERLIN, "data/precompile-modexp.json"),
    Bench::transaction_suite(SpecId::OSAKA, "data/precompile-p256verify.json"),
    Bench::transaction_suite(SpecId::OSAKA, "data/precompile-ripemd160.json"),
    Bench::transaction_suite(SpecId::OSAKA, "data/precompile-sha256.json"),
    Bench::blockchain_replay("mainnet_25347446_25347455", "data/mainnet-25347446-25347455.bin.zst"),
];
