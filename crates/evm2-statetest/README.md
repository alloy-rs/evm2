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
EVM2_STATETEST_JOBS=28 cargo nextest run -p evm2-statetest --test statetest -j28
```

`EVM2_STATETEST_JOBS` controls file-level parallelism inside the single nextest
test and defaults to at most 28 workers.

Run one subdirectory across all downloaded suites:

```sh
SUBDIR=stRevertTest EVM2_STATETEST_JOBS=28 cargo nextest run -p evm2-statetest --test statetest -j28
```

Run an explicit root:

```sh
EVM2_STATETEST_ROOT=test-fixtures/legacytests/Constantinople/GeneralStateTests \
SUBDIR=stExample \
EVM2_STATETEST_JOBS=28 \
cargo nextest run -p evm2-statetest --test statetest -j28
```
