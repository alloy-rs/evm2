use evm2::SpecId;

pub(crate) struct Bench {
    pub(crate) name: &'static str,
    pub(crate) spec: SpecId,
    pub(crate) fixture_path: &'static str,
}

pub(crate) const fn all() -> &'static [Bench] {
    &[
        Bench {
            name: "fibonacci-calldata",
            spec: SpecId::OSAKA,
            fixture_path: "data/fibonacci-calldata.json",
        },
        Bench { name: "factorial", spec: SpecId::OSAKA, fixture_path: "data/factorial.json" },
        Bench { name: "counter", spec: SpecId::OSAKA, fixture_path: "data/counter.json" },
        Bench { name: "snailtracer", spec: SpecId::CANCUN, fixture_path: "data/snailtracer.json" },
        Bench { name: "weth", spec: SpecId::OSAKA, fixture_path: "data/weth.json" },
        Bench { name: "hash_10k", spec: SpecId::OSAKA, fixture_path: "data/hash_10k.json" },
        Bench {
            name: "erc20_transfer",
            spec: SpecId::OSAKA,
            fixture_path: "data/erc20_transfer.json",
        },
        Bench { name: "push0_proxy", spec: SpecId::OSAKA, fixture_path: "data/push0_proxy.json" },
        Bench { name: "usdc_proxy", spec: SpecId::OSAKA, fixture_path: "data/usdc_proxy.json" },
        Bench { name: "fiat_token", spec: SpecId::OSAKA, fixture_path: "data/fiat_token.json" },
        Bench {
            name: "uniswap_v2_pair",
            spec: SpecId::OSAKA,
            fixture_path: "data/uniswap_v2_pair.json",
        },
        Bench { name: "univ2_router", spec: SpecId::OSAKA, fixture_path: "data/univ2_router.json" },
        Bench { name: "seaport", spec: SpecId::OSAKA, fixture_path: "data/seaport.json" },
        Bench { name: "airdrop", spec: SpecId::OSAKA, fixture_path: "data/airdrop.json" },
        Bench { name: "bswap64", spec: SpecId::OSAKA, fixture_path: "data/bswap64.json" },
        Bench { name: "bswap64_opt", spec: SpecId::OSAKA, fixture_path: "data/bswap64_opt.json" },
        Bench { name: "eip4788", spec: SpecId::OSAKA, fixture_path: "data/eip4788.json" },
        Bench { name: "eip2935", spec: SpecId::OSAKA, fixture_path: "data/eip2935.json" },
        Bench { name: "burntpix", spec: SpecId::CANCUN, fixture_path: "data/burntpix.json" },
        Bench {
            name: "curve_stableswap",
            spec: SpecId::CANCUN,
            fixture_path: "data/curve-stableswap-2pool.json",
        },
        Bench {
            name: "onchain_lm_v2",
            spec: SpecId::CANCUN,
            fixture_path: "data/onchain-lm-v2.json",
        },
    ]
}
