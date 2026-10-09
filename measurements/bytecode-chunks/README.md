# Bytecode chunking experiment

Prototype in `rakita/bytecode-chunking`, based on EVM2 `4a8ced3868c5d58f7cebfa2b7b9972a698f4a352`.
Measured 2026-10-08 on Apple M5 Max, macOS 26.7.1, rustc 1.98.1, release profile,
packed interpreter dispatch. These measurements use the existing gas schedule.

## Current 12 KiB prototype

`CODE_CHUNK_SIZE = 12_288`. Original payloads are short and unpadded. Deployment
validation rejects a PUSH immediate that crosses a boundary and requires every
payload to end in a decoded STOP. Chunks are validated once when bytecode is
created or transitioned; trusted database reads do not repeat that validation.
The required `get_code_chunk_by_hash(hash, index)` method is implemented across
typed, erased, async, cache, and counted DB adapters. There is no full-code fetch
fallback. In-memory backends may split already resident code once and cache it.

`State::load_code_chunk(address, index, skip_cold_load)` returns bytes and prior
warmth. The immutable cache is keyed by `(hash, index)`; gas warmth is keyed by
`(address, hash, index)`. Bytes survive reverts and transaction resets. Warmth is
journaled, reverts to the checkpoint, and resets per transaction. Snapshots keep
all three structures. Uncommitted deployed/replaced code takes precedence over
the accepted-state database. Failed, skipped, and absent reads do not warm.

The proposed incremental gas function is
`28680 * cold_chunk_count + 1000 * warm_chunk_count`, checked for overflow and
reserved before chunk I/O. At the requested 1 Ggas/s calibration, a cold access
prices 28.68 microseconds for lookup, reading, construction, and analysis; a warm
access prices one microsecond for the account-map lookup and active-chunk switch.
Account/opcode/memory/copy charges remain separate. These are draft consensus
constants pending measurement against Tempo's persistent provider.

### Ten-block replay at 12 KiB

| Measure | Result |
|---|---:|
| Full code bytes (sum of per-block unions) | 29,955,810 |
| Covered original bytes | 29,493,944 |
| Chunk payload bytes | 29,493,944 |
| Payload reduction | **1.54%** |
| Touched / total chunks | 3,963 / 4,053 |
| Transaction-local full code / payload | 66,668,187 / 65,950,134 |
| Transaction-local reduction | 1.08% |
| Baseline replay median | 101.584 ms |
| No-op inspector median | 115.924 ms |
| Coverage inspector median | 184.770 ms (1.819× baseline) |

All 2,279 results match baseline and receipt roots pass, under unchanged gas.
This workload gets substantially less payload reduction at 12 KiB than at the
previous small sizes, with far fewer logical chunks. The current unpadded final
chunk accounting differs from historical padded payloads. The stored CSVs make
that distinction explicit through covered bytes and touched chunks.

These remain coverage/tracing measurements. The native interpreter draft now
loads chunk zero for calls and transaction entry, swaps chunks on cross-boundary
JUMP/JUMPI, maintains global PC/CODESIZE, and charges cold and warm chunk access.
The stored replay was recorded before that execution path was enabled and does
not measure physical I/O or gas deltas. Persistent storage migration, provider
integration, and Tempo replay measurement remain incomplete. Chunked frames
currently bypass JIT/AOT execution and use the interpreter.

Reproduce the current measurements (the filenames without `12k` retain earlier
small-chunk results):

```sh
EVM2_DISPATCH_BACKEND=packed cargo run -q --release -p evm2-cli --example bytecode_chunks_mainnet > measurements/bytecode-chunks/mainnet-12k-coverage.csv 2> measurements/bytecode-chunks/mainnet-12k-timings.txt
EVM2_DISPATCH_BACKEND=packed cargo run -q --release -p evm2-cli --example bytecode_chunks > measurements/bytecode-chunks/12k-coverage.csv 2> measurements/bytecode-chunks/12k-timings.txt
```

The companion Tempo draft is TIP-1143, on branch `tip/1143`.

## Historical small-chunk replay

The bundled fixture covers Ethereum blocks 25,347,446–25,347,455: 2,279 transactions,
2,237 successful and 42 unsuccessful. Every traced replay matched all baseline
transaction results (status, gas, output, logs and other result fields). Receipt
roots were checked against the fixture headers on every replay. The EEST runner
also retained its default post-state validation; full state-root comparison was
not enabled.

Code reads are deduplicated by `(code address, code hash, chunk index)` within each
block. The table sums the ten block totals; contracts recurring in different
blocks are counted again. The full-code denominator is the original code length
of contracts executed or successfully accessed with EXTCODECOPY. It is not a
measurement of physical disk reads or cache misses.

| Code bytes per chunk | Full code bytes | Chunk payload bytes | Reduction | Touched chunks |
|---:|---:|---:|---:|---:|
| 31 | 29,955,810 | 7,670,144 | 74.40% | 239,692 |
| 63 | 29,955,810 | 8,959,168 | 70.09% | 139,987 |
| 127 | 29,955,810 | 10,849,152 | 63.78% | 84,759 |
| 255 | 29,955,810 | 13,623,040 | 54.52% | 53,215 |

Payload includes one metadata byte per chunk and last-chunk padding, but excludes
Merkle proofs, account metadata and database overhead. This earlier run used a 31-byte codec; other sizes were coverage/payload estimates.
The current codec has since been replaced by the 12 KiB design below. Proof sizes must be
measured before choosing a chunk size.

Summing transaction-local coverage instead gives 66,668,187 full-code bytes versus
14,508,448 chunk payload bytes for 31-byte chunks: 78.24% less. This counts a
contract again each time a different transaction uses it.

## Runtime overhead

Seven interleaved samples per mode, reporting the median. Input fixture decoding
is outside the timer. Mainnet timing includes replay setup, transaction decoding,
execution, commits and receipt-root validation. Coverage aggregation/reporting is
disabled during timing. Each transaction gets a fresh inspector.

| Mode | Ten-block replay | Range across seven samples |
|---|---:|---:|
| No inspector | 103.241 ms | 102.288–104.333 ms |
| Empty inspector | 118.401 ms | — |
| Chunk inspector, 31-byte payload | 188.958 ms | 187.763–189.091 ms |

The tracer is **1.83× baseline** and **1.60× the empty inspector**. This is the cost
of this per-opcode inspector implementation, not a prediction for a native
chunk-aware interpreter. No execution speedup, I/O saving or new gas schedule is
claimed. The next implementation experiment should measure a native tracker that
avoids repeated marking within an already accessed chunk, plus actual proof costs.

## What is implemented

- `crates/evm2/src/bytecode/chunks.rs`: creation validation and independently
  executable 12 KiB chunks ending in a decoded STOP.
- The journaled account records complete code size and a map of loaded bytecode
  chunks. Cross-chunk jumps fetch, validate, activate, and meter their target.
- `crates/inspectors/src/code_chunks.rs`: configurable coverage inspector for
  instruction fetches, PUSH payloads, jump targets, CODECOPY and EXTCODECOPY.
- A successful-EXTCODECOPY inspector hook observes already loaded code without
  performing extra database or host accesses.
- EEST transaction lifecycle hooks let the mainnet benchmark install and collect
  inspectors without including pre/post-block system calls.
- The two CLI examples below produce raw coverage and timing measurements.

Reads from reverted frames remain in coverage. Delegate calls are attributed to
the code address, not the storage address. Copy ranges are clipped to original
code length. Untaken jumps, out-of-range targets and implicit padded STOP bytes
do not introduce extra chunks. Operand/source ranges are recorded after successful
execution, with invalid jump targets explicitly included.

This remains a draft rather than an activation-ready implementation. It includes
the proposed cold and warm gas in the feature-gated interpreter path, on-demand
CODECOPY and EXTCODECOPY, and EIP-7702 chunk-zero handling, but lacks proof
generation, state migration, persistent provider writes, and Tempo replay.
Compiled execution remains disabled for chunked frames.

The smaller contract fixtures are microbenchmarks, not full application workloads:
fiat_token queries decimals, uniswap_v2_pair reads reserves, and usdc_proxy uses
empty calldata. Their raw results are retained for comparison, not extrapolation.

## Validation and environment

Use the current reproduction commands above with the normal native dependencies
(including CMake) on PATH.

Backend selection happens at build time. The small-fixture runner accepts optional
fixture names as positional arguments. Small-fixture timings exclude fresh-EVM
setup and DB cloning and therefore are not directly comparable to replay timings.

Validation completed:

```sh
cargo test -p evm2-inspectors --tests
cargo test -p evm2 --no-default-features --features std,async,serde --lib
EVM2_DISPATCH_BACKEND=packed cargo clippy -p evm2-cli --example bytecode_chunks --example bytecode_chunks_mainnet -p evm2-inspectors --lib -- -D warnings
cargo +nightly fmt --all --check
```

Current focused tests validate boundary rejection, terminal STOP requirements,
cross-chunk jumps, exact cold/warm gas delta, metadata queries, and rollback of
warmth, loaded maps, and code replacement. The observational inspector suites
continue to cover invalid targets, untaken branches, copy bounds, delegation,
and reverted reads.
The replay checks 2,279 transaction results for the current 12 KiB chunk size;
small fixtures additionally compare detached state diffs.

Local Cargo.lock SHA-256: `f502da7e1e2b1f3960581b3e94a7b4684e9be37439ffcbf29f8f3cfede9dc937`.
Fixture SHA-256: `506458bf10ef81c13e821c99b36105b3471172bf7b9ce112410b8a7ba5d45a9e`.
