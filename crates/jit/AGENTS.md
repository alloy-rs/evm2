# evm2-jit - EVM JIT/AOT compiler

## Commands

Use the root commands for lint, format, and docs. Targeted JIT commands:

```bash
cargo nextest run -p evm2-jit-codegen --test codegen                             # compiler tests
cargo nextest run -p evm2-jit-codegen --test codegen "test_name"                 # single compiler test
EVM2_JIT_TEST_DUMP=1 cargo nextest run -p evm2-jit-codegen --test codegen "test_name" # dump single compiler test
cargo nextest run -p evm2-jit-runtime                                            # runtime tests
cargo st "statetests::devnet::jit"                                               # EEST JIT state tests
cargo st "blockchain_tests::devnet::jit"                                         # EEST JIT blockchain tests
```

## Architecture

- `evm2-jit` — thin umbrella crate that re-exports codegen and runtime APIs.
- `evm2-jit-codegen` — EVM compiler, bytecode analysis, linker, and compiler test infrastructure.
- `evm2-jit-runtime` — runtime JIT/AOT backend, worker pool, artifact store, and evm2 integration.
- `evm2-jit-backend` — abstract compiler backend trait. `evm2-jit-llvm` is the main implementation.
- `evm2-jit-builtins` — runtime builtins called by JIT-compiled code (host calls, gas accounting).
- `evm2-jit-context` — EVM execution context types bridging evm2 and compiled code.
- `evm2-jit-build` — build-script helpers for AOT compilation.

## CLI

Do NOT use `--release` — dev profile already uses `opt-level = 3`, and release
changes panic/debug behavior and makes local iteration slower. Use the
`profiling` profile when symbolized optimized builds are needed.

```bash
cargo cli list <fixture.json>             # list replay entrypoints
cargo cli replay --jit <fixture.json>     # replay through JIT
cargo cli replay --aot <fixture.json>     # replay through AOT
```

When a compiler dump directory is configured, common files are:

- `bytecode.bin` — raw input bytecode.
- `bytecode.txt` — parsed bytecode IR with blocks, gas, stack info, and comments.
- `bytecode.dbg.txt` — verbose debug dump of the parsed bytecode structure.
- `bytecode.dot` / `bytecode.svg` — rendered CFG.
- `unopt.ll` — LLVM IR before optimization.
- `opt.ll` — optimized LLVM IR.
- `opt.s` — final optimized assembly.
- `remarks.txt` — compile timings, JIT size, and generated-file sizes.

Use `RUST_LOG` to control log output:

```bash
RUST_LOG=debug cargo cli replay --jit <fixture.json>
RUST_LOG=evm2_jit_codegen=trace cargo cli replay --jit <fixture.json>
```

## Injecting LLVM args

Extra LLVM command-line arguments can be passed via the `EVM2_JIT_LLVM_ARGS`
environment variable (space-separated):

```bash
EVM2_JIT_LLVM_ARGS="-debug-only=isel" cargo cli replay --jit <fixture.json>
EVM2_JIT_LLVM_ARGS="-print-after-all" cargo cli replay --jit <fixture.json>
```

LLVM args are a one-shot global (`LLVMParseCommandLineOptions`); only the first
call takes effect.

Use `EVM2_JIT_PASSES` to override the LLVM optimization pass pipeline for local
experiments.

## Checking dynamic jump resolution

To get jump resolution stats across benchmarks:

```bash
./scripts/jit/bench.py /tmp/bench --jump-resolution                    # all benchmarks
./scripts/jit/bench.py /tmp/bench --jump-resolution usdc_proxy weth    # specific benchmarks
```

To inspect a single contract in detail:

```bash
RUST_LOG=evm2_jit_codegen::bytecode=trace ./scripts/jit/bench.py /tmp/bench --jump-resolution usdc_proxy |& rg 'jump|JUMP'
```

- `local_jumps.*newly_resolved=N` - jumps resolved locally.
- `resolved non-adjacent jump` - jump target resolved outside adjacent layout.
- `resolved via PCR hint` - jump target resolved through a pushed-code-region hint.
- `unresolved dynamic jumps remain n=N` — jumps that couldn't be resolved.
- `JUMP bb<N>` / `JUMP bb<N>, bb<M>` — resolved (single/multi-target).
- `JUMP ; pc=<N>` — unresolved dynamic jump.

## Benchmarking against another revision

`./scripts/jit/bench.py` is the unified benchmarking tool. It collects codegen
line counts, compile times, jump resolution stats, and constant-input
statistics.

The script writes its full markdown output to `<dump_dir>/results.md` in
addition to printing it to stdout. Summary tables hide changes within a noise
threshold (1% for codegen, 5% for compile times); the `<details>` tables still
show every change.

```bash
./scripts/jit/bench.py /tmp/bench --diff <base-rev>                    # codegen + compile time vs base
./scripts/jit/bench.py /tmp/bench --diff <base-rev> usdc_proxy seaport # specific benchmarks
./scripts/jit/bench.py /tmp/bench --diff <base-rev> --extra-dir tmp/mainnet # include mainnet .bin files
./scripts/jit/bench.py /tmp/bench                                      # current branch only (no diff)
./scripts/jit/bench.py /tmp/bench --diff <base-rev> --compile-times    # compile times only
./scripts/jit/bench.py /tmp/bench --diff <base-rev> --codegen-lines    # codegen lines only
./scripts/jit/bench.py /tmp/bench --jump-resolution                    # jump resolution stats
./scripts/jit/bench.py /tmp/bench --input-stats                        # constant-input stats
./scripts/jit/bench.py /tmp/bench --block-stats                        # block stats
./scripts/jit/bench.py /tmp/bench --codegen-lines --jump-resolution    # combine multiple analyses
```

Use a base revision that already contains the JIT CLI `run` command.

## Bench-and-PR workflow

When the user asks to "bench and open pr", "post results to pr", or whenever
making a perf change that needs benchmark numbers in the PR description:

1. Run `./scripts/jit/bench.py <dump_dir> --diff <base>` with a base revision that contains the JIT CLI `run` command.
2. Build the PR body **in a single bash command** that inlines
   `<dump_dir>/results.md` VERBATIM. Do NOT reformat, summarize, drop
   columns, or rewrite the numbers in the tables — `cat` the file as-is.
3. Add prose explaining what the PR does ABOVE the inlined results, under a
   `## Benchmarks` (or similar) heading.
4. Under `## Benchmarks`, ABOVE the inlined `results.md`, write a short
   textual summary of the headline numbers (e.g. the `**TOTAL**` row diffs
   from the codegen + compile-time tables, plus any notable per-bench wins
   or regressions worth calling out). Keep it to a few sentences or a tight
   bullet list — this is the at-a-glance summary readers see before the
   tables. The tables themselves stay verbatim.
5. Update the PR with `gh pr edit <number> --body-file <body.md>`.

Example — write the body file with prose, summary, and verbatim results in
one shot:

```bash
{
  cat <<'EOF'
Short description of what this PR does and why.

More prose: motivation, design notes, caveats, anything reviewers need.

## Benchmarks

Headline numbers vs `main`: jit size -7.5%, opt.s +2.9%, total compile time
+0.2%. `counter` regresses on opt.s (+26%); `seaport` is roughly flat.

EOF
  cat /tmp/bench/results.md
} > /tmp/pr-body.md

gh pr edit 123 --body-file /tmp/pr-body.md
```

The heredoc holds whatever prose + summary belongs in the PR; `cat results.md`
appends the benchmark tables exactly as the script produced them.

## Important

- NEVER alter or summarize the benchmark tables themselves — always post them
  verbatim. A short textual summary of the headline numbers ABOVE the tables
  (under `## Benchmarks`) is required.
- NEVER delete or modify `./tmp/` — it contains manually generated IR/asm dumps used for comparison.
- `tmp/dump/` contains dumps from `main`, `tmp/dump2/` contains dumps from the current branch.
  Use these for manual `diff` comparison of LLVM IR and assembly.

## Code style

- Never call `.index()` on an index type just to reconstruct the same type.
  Use arithmetic on the index directly:
  ```rust
  // BAD
  let prev = &self.insts[Inst::from_usize(term_inst.index() - 1)];
  // GOOD
  let prev = &self.insts[term_inst - 1];
  ```
- Don't prefix log messages with the pass/function name — `#[instrument]` spans
  already provide that context. Just describe what happened:
  ```rust
  // BAD
  trace!("local: resolved jump");
  // GOOD
  trace!("resolved jump");
  ```
