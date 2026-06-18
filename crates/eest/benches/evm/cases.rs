use evm2::SpecId;

pub(crate) struct Bench {
    pub(crate) name: &'static str,
    pub(crate) fixture_path: &'static str,
    pub(crate) kind: BenchKind,
}

#[derive(Clone, Copy)]
pub(crate) enum BenchKind {
    Transaction { spec: SpecId },
    BlockchainReplay,
}

impl Bench {
    pub(crate) const fn transaction(
        name: &'static str,
        spec: SpecId,
        fixture_path: &'static str,
    ) -> Self {
        Self { name, fixture_path, kind: BenchKind::Transaction { spec } }
    }

    pub(crate) const fn blockchain_replay(name: &'static str, fixture_path: &'static str) -> Self {
        Self { name, fixture_path, kind: BenchKind::BlockchainReplay }
    }

    pub(crate) const fn transaction_spec(&self) -> Option<SpecId> {
        match self.kind {
            BenchKind::Transaction { spec } => Some(spec),
            BenchKind::BlockchainReplay => None,
        }
    }

    pub(crate) const fn transaction_fixture_path(&self) -> Option<&'static str> {
        match self.kind {
            BenchKind::Transaction { .. } => Some(self.fixture_path),
            BenchKind::BlockchainReplay => None,
        }
    }
}

pub(crate) const fn all() -> &'static [Bench] {
    BENCHES
}

static BENCHES: &[Bench] = &[
    Bench::transaction("fibonacci-calldata", SpecId::OSAKA, "data/fibonacci-calldata.json"),
    Bench::transaction("factorial", SpecId::OSAKA, "data/factorial.json"),
    Bench::transaction("counter", SpecId::OSAKA, "data/counter.json"),
    Bench::transaction("snailtracer", SpecId::CANCUN, "data/snailtracer.json"),
    Bench::transaction("weth", SpecId::OSAKA, "data/weth.json"),
    Bench::transaction("hash_10k", SpecId::OSAKA, "data/hash_10k.json"),
    Bench::transaction("erc20_transfer", SpecId::OSAKA, "data/erc20_transfer.json"),
    Bench::transaction("push0_proxy", SpecId::OSAKA, "data/push0_proxy.json"),
    Bench::transaction("usdc_proxy", SpecId::OSAKA, "data/usdc_proxy.json"),
    Bench::transaction("fiat_token", SpecId::OSAKA, "data/fiat_token.json"),
    Bench::transaction("uniswap_v2_pair", SpecId::OSAKA, "data/uniswap_v2_pair.json"),
    Bench::transaction("univ2_router", SpecId::OSAKA, "data/univ2_router.json"),
    Bench::transaction("seaport", SpecId::OSAKA, "data/seaport.json"),
    Bench::transaction("airdrop", SpecId::OSAKA, "data/airdrop.json"),
    Bench::transaction("bswap64", SpecId::OSAKA, "data/bswap64.json"),
    Bench::transaction("bswap64_opt", SpecId::OSAKA, "data/bswap64_opt.json"),
    Bench::transaction("eip4788", SpecId::OSAKA, "data/eip4788.json"),
    Bench::transaction("eip2935", SpecId::OSAKA, "data/eip2935.json"),
    Bench::transaction("burntpix", SpecId::CANCUN, "data/burntpix.json"),
    Bench::transaction("curve_stableswap", SpecId::CANCUN, "data/curve-stableswap-2pool.json"),
    Bench::transaction("onchain_lm_v2", SpecId::CANCUN, "data/onchain-lm-v2.json"),
    Bench::blockchain_replay("mainnet_25347446_25347455", "data/mainnet-25347446-25347455.bin.zst"),
];
