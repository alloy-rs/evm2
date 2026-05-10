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

## State tests

Run all discovered state tests with nextest:

```sh
cargo nextest run -p evm2-eest --test statetest --ignore-default-filter -j28
```

By default, this runs `main/develop/state_tests` plus the legacy Cancun and
Constantinople state tests. EEST develop includes the stable fixtures, so stable
is not run separately.

Set `EVM2_STATETEST_STABLE=1` to download and run EEST stable fixtures instead
of develop.

## Blockchain tests

Run all discovered blockchain tests with nextest:

```sh
cargo nextest run -p evm2-eest --test blockchaintest --ignore-default-filter -j28
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

Run one subdirectory across all downloaded suites:

```sh
SUBDIR=stRevertTest cargo nextest run -p evm2-eest --test statetest --ignore-default-filter -j28
SUBDIR=berlin cargo nextest run -p evm2-eest --test blockchaintest --ignore-default-filter -j28
```

Run one explicit test file:

```sh
cargo nextest run -p evm2-eest --test statetest -j28 \
  --ignore-default-filter legacy_constantinople::stExample/add11.json

cargo nextest run -p evm2-eest --test blockchaintest -j28 \
  --ignore-default-filter eest::berlin/eip2929_gas_cost_increases/test_call_insufficient_balance.json
```

List all discovered tests:

```sh
cargo nextest list -p evm2-eest --test statetest --ignore-default-filter
cargo nextest list -p evm2-eest --test blockchaintest --ignore-default-filter
```

For local experiments, `EVM2_STATETEST_ROOT`, `EVM2_BLOCKCHAINTEST_ROOT`,
`EVM2_STATETEST_STABLE`, `EVM2_BLOCKCHAINTEST_STABLE`, `EVM2_EEST_STABLE`,
`ETHEREUM_TESTS`, `ETHTESTS`, `EVM2_TEST_FIXTURES`, `REVMC_TEST_FIXTURES`, and
`SUBDIR` are supported as optional filters.
