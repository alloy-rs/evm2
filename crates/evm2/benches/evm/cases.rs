use evm2::SpecId;

const REVMC_BYTECODE_FIXTURES: &str = "data/revmc-bytecode.json";

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
            fixture_path: REVMC_BYTECODE_FIXTURES,
        },
        Bench { name: "factorial", spec: SpecId::OSAKA, fixture_path: REVMC_BYTECODE_FIXTURES },
        Bench { name: "counter", spec: SpecId::OSAKA, fixture_path: REVMC_BYTECODE_FIXTURES },
        Bench { name: "snailtracer", spec: SpecId::CANCUN, fixture_path: "data/snailtracer.json" },
        Bench { name: "weth", spec: SpecId::OSAKA, fixture_path: REVMC_BYTECODE_FIXTURES },
        Bench { name: "hash_10k", spec: SpecId::OSAKA, fixture_path: REVMC_BYTECODE_FIXTURES },
        Bench {
            name: "erc20_transfer",
            spec: SpecId::OSAKA,
            fixture_path: REVMC_BYTECODE_FIXTURES,
        },
        Bench { name: "push0_proxy", spec: SpecId::OSAKA, fixture_path: REVMC_BYTECODE_FIXTURES },
        Bench { name: "usdc_proxy", spec: SpecId::OSAKA, fixture_path: REVMC_BYTECODE_FIXTURES },
        Bench { name: "fiat_token", spec: SpecId::OSAKA, fixture_path: REVMC_BYTECODE_FIXTURES },
        Bench {
            name: "uniswap_v2_pair",
            spec: SpecId::OSAKA,
            fixture_path: REVMC_BYTECODE_FIXTURES,
        },
        Bench { name: "univ2_router", spec: SpecId::OSAKA, fixture_path: REVMC_BYTECODE_FIXTURES },
        Bench { name: "seaport", spec: SpecId::OSAKA, fixture_path: REVMC_BYTECODE_FIXTURES },
        Bench { name: "airdrop", spec: SpecId::OSAKA, fixture_path: REVMC_BYTECODE_FIXTURES },
        Bench { name: "bswap64", spec: SpecId::OSAKA, fixture_path: REVMC_BYTECODE_FIXTURES },
        Bench { name: "bswap64_opt", spec: SpecId::OSAKA, fixture_path: REVMC_BYTECODE_FIXTURES },
        Bench { name: "eip4788", spec: SpecId::OSAKA, fixture_path: REVMC_BYTECODE_FIXTURES },
        Bench { name: "eip2935", spec: SpecId::OSAKA, fixture_path: REVMC_BYTECODE_FIXTURES },
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
