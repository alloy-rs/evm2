# evm2-eest

Ethereum Execution Spec Test runners for evm2.

The default fixture layout is repo-relative:

```text
test-fixtures/
├── main/
│   ├── stable/{state_tests,blockchain_tests}/...
│   └── develop/{state_tests,blockchain_tests}/...
├── devnet/
│   └── {state_tests,blockchain_tests}/...
└── legacytests/
    ├── Cancun/GeneralStateTests/...
    └── Constantinople/GeneralStateTests/...
```

Populate it with:

```sh
./scripts/setup_test_fixtures.py
```

Run all discovered EEST fixtures with the single `eest` test binary:

```sh
cargo nextest run -p evm2-eest --test eest --ignore-default-filter
```

Discovered tests are named under `statetests`, `blockchain_tests`, and `legacy`.

## State tests

Run all discovered state tests with nextest:

```sh
cargo nextest run -p evm2-eest --test eest --ignore-default-filter statetests
```

By default, this runs `main/develop/state_tests` plus the legacy Cancun and
Constantinople state tests. EEST develop includes the stable fixtures, so stable
is not run separately.

Set `EVM2_STATETEST_STABLE=1` to download and run EEST stable fixtures instead
of develop.

## Blockchain tests

Run all discovered blockchain tests with nextest:

```sh
cargo nextest run -p evm2-eest --test eest --ignore-default-filter blockchain_tests
```

By default, this runs `main/develop/blockchain_tests` and
`devnet/blockchain_tests`. Transition forks are skipped. Blocks with
`blockAccessList` currently reach a `todo!()` assertion until evm2 can build
block access lists.

Set `EVM2_BLOCKCHAINTEST_STABLE=1` or `EVM2_EEST_STABLE=1` to use EEST stable
instead of develop for main blockchain fixtures.

## Filtering

The default nextest profile excludes this crate from workspace runs. Pass
`--ignore-default-filter` when running EEST tests explicitly.

Filter by top-level suite name:

```sh
cargo nextest run -p evm2-eest --test eest --ignore-default-filter statetests
cargo nextest run -p evm2-eest --test eest --ignore-default-filter blockchain_tests
cargo nextest run -p evm2-eest --test eest --ignore-default-filter legacy
```

Run one subdirectory across all downloaded suites:

```sh
SUBDIR=stRevertTest cargo nextest run -p evm2-eest --test eest --ignore-default-filter statetests
SUBDIR=berlin cargo nextest run -p evm2-eest --test eest --ignore-default-filter blockchain_tests
```

Run one explicit test file:

```sh
cargo nextest run -p evm2-eest --test eest \
  --ignore-default-filter legacy::constantinople::stExample/add11.json

cargo nextest run -p evm2-eest --test eest \
  --ignore-default-filter blockchain_tests::berlin/eip2929_gas_cost_increases/test_call_insufficient_balance.json
```

List all discovered tests:

```sh
cargo nextest list -p evm2-eest --test eest --ignore-default-filter
```

For local experiments, `EVM2_STATETEST_ROOT`, `EVM2_BLOCKCHAINTEST_ROOT`,
`EVM2_STATETEST_STABLE`, `EVM2_BLOCKCHAINTEST_STABLE`, `EVM2_EEST_STABLE`,
`EVM2_TEST_FIXTURES`, and `SUBDIR` are supported as optional filters.

Test cases for unsupported hardforks (currently Amsterdam) are skipped
automatically. Set `EVM2_SKIP_FORKS` to a comma-separated list of hardfork
names (for example `EVM2_SKIP_FORKS=osaka,prague`) to skip additional full
hardforks.
