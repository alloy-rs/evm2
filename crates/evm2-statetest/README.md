# evm2-statetest

Ethereum state test runner for evm2.

The default fixture layout is repo-relative and matches revm/revmc:

```text
test-fixtures/
├── main/
│   ├── stable/state_tests/...
│   └── develop/state_tests/...
└── legacytests/
    ├── Cancun/GeneralStateTests/...
    └── Constantinople/GeneralStateTests/...
```

Populate it with:

```sh
./scripts/setup-test-fixtures.sh
```

The script downloads into `test-fixtures` and exits early for suites that are
already present.

Run all discovered state tests with nextest:

```sh
cargo nextest run -p evm2-statetest --test statetest -j28
```

Each JSON file is listed as a separate nextest test. `-j` controls how many
state test files nextest runs concurrently. In CI we run a smoke test from this
default layout without passing state-test environment variables.

Run one subdirectory across all downloaded suites:

```sh
SUBDIR=stRevertTest cargo nextest run -p evm2-statetest --test statetest -j28
```

Run one explicit test file:

```sh
cargo nextest run -p evm2-statetest --test statetest -j28 \
  legacy_constantinople::stExample/add11.json
```

List all discovered state test files:

```sh
cargo nextest list -p evm2-statetest --test statetest
```

For local experiments, `EVM2_STATETEST_ROOT`, `ETHEREUM_TESTS`, `ETHTESTS`,
`EVM2_TEST_FIXTURES`, `REVMC_TEST_FIXTURES`, and `SUBDIR` are still supported as
optional compatibility filters.
