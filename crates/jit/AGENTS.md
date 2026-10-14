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
addition to printing it to stdout. Summary tables hide rows within a noise
threshold (1% for codegen and instructions, 5% for time) and end with a
`TOTAL` row and an equal-weight `GEOMEAN` row; the `<details>` tables still
show every value. Benchmarks that fail on either side go in a `Failures`
table instead of aborting the run.

```bash
./scripts/jit/bench.py /tmp/bench --diff <base-rev>                    # codegen + compile time vs base
./scripts/jit/bench.py /tmp/bench --diff <base-rev> usdc_proxy seaport # specific benchmarks
./scripts/jit/bench.py /tmp/bench --diff <base-rev> --extra-dir tmp/mainnet # include mainnet .bin files
./scripts/jit/bench.py /tmp/bench                                      # current branch only (no diff)
./scripts/jit/bench.py /tmp/bench --diff <base-rev> --compile-times    # compile times only
./scripts/jit/bench.py /tmp/bench --diff <base-rev> --codegen-lines    # codegen lines only
./scripts/jit/bench.py /tmp/bench --diff <base-rev> --runs 5           # more compile-cost samples
./scripts/jit/bench.py /tmp/bench --jump-resolution                    # jump resolution stats
./scripts/jit/bench.py /tmp/bench --input-stats                        # constant-input stats
./scripts/jit/bench.py /tmp/bench --block-stats                        # block stats
./scripts/jit/bench.py /tmp/bench --diff <base-rev> --runtime          # execution instructions per iteration
./scripts/jit/bench.py /tmp/bench --codegen-lines --jump-resolution    # combine multiple analyses
```

Use a base revision that already contains the JIT CLI `run` command. Prefer
`origin/main` after `git fetch` over `main`: a stale local `main` makes the
diff include unrelated commits. The first line of `results.md` names both
commits.

With `--diff`, the script builds the base in a git worktree at
`<dump_dir>.base/.worktree` with its own target dir, `<dump_dir>.base/target`,
and caches the binary there per commit, so later runs against the same base
rebuild only the head. Sharing the head's target dir would let Cargo treat
head files older than the base build as fresh. The worktree must stay: the
CLI reads fixtures from the source tree it was built from. Delete
`<dump_dir>.base` and run `git worktree prune` to drop the cache.

The codegen section starts with the number of benchmarks whose `opt.s` changed,
ignoring dump paths. `0/N` shows that a refactor leaves codegen alone.

### Compile times

- `instructions` and `time` are medians of `--runs` (default 3) runs of
  `evm2 run <bench> 0`, which JIT-compiles every contract in the fixture
  without dumps or execution. `instructions` counts user-space instructions
  with `perf stat` and varies by about 0.3% between runs. `time` is CPU time
  (wall time without `perf`); on a busy machine it moved by more than 10% for
  identical code. Head and base runs alternate, so load hits both sides
  alike. Judge compile-time changes by `instructions`.
- `parse`, `translate`, `optimize`, and `codegen` come from the single dump
  run's `remarks.txt` and appear only in the details table. They are noisy:
  use them to find which phase moved, not by how much.
- Do not read `finalize` or `total` in `remarks.txt` as compile time.
  `finalize` includes writing `unopt.ll` and `opt.ll` and generating `opt.s`;
  seaport compiles in 2.4s without dumps but reports 4.4s total with them.

### Runtime

`--runtime` counts user-space instructions per JIT execution of each benchmark:
`perf stat` over `evm2 run <bench> N --jit-only`, minus a compile-only run, with
`N` chosen so execution dominates. Like CodSpeed's simulation mode, the count is
deterministic, so it works on a busy machine, but it does not see cache misses or
branch mispredictions. It is opt-in because it takes a few minutes; add the
default analyses explicitly (`--codegen-lines --compile-times --runtime`) to get
all three. The CodSpeed job in `.github/workflows/bench.yml` still has the final
word on runtime; cite its report for runtime claims.

### Codegen settings

All runs pass `--no-frame-pointers`. The dev-profile CLI enables frame pointers
through `debug_assertions`, and dumps force them on, but release builds keep
them off; with them on, the dumped assembly has one register fewer and 38%
more spills than the code that actually runs.

### LLVM experiments

`EVM2_JIT_PASSES` and `EVM2_JIT_LLVM_ARGS` let you try pass pipelines and
flags without rebuilding, but `bench.py` passes the environment to both sides.
To diff a pipeline change, make it in code. `EVM2_JIT_PASSES` also replaces the
pipeline for every function size, which drops the LICM size gate in
`optimize_module`. Passing an option that `init_` already sets, such as
`--huge-size-for-split`, does not reliably override it.

Experiments that did not pay off, so you don't repeat them blindly:

- Keeping the remaining gas in a local (`alloca`) synced with `ecx.gas` around
  calls: x86 turns `sub qword ptr [ecx], imm; jb` into `cmp`/`lea` on a
  register that stays live everywhere, so spills went up 72%.
- `preserve_most` stubs for host builtins (`SLOAD`, `CALL`, ...): fewer spills
  in some contracts, many more reloads in others, about neutral overall.
- Folding the memory offset range check into the resize branch: LLVM merges
  the conditions back into the same compare chain and the cold path grows.
- `early-cse<memssa>`: forwards stack loads across blocks but raises register
  pressure; erc20_transfer spills went from 12 to 87.

### CI

The `jit` job in `.github/workflows/bench.yml` runs `bench.py --output
--runtime` on every push to `main` and every PR; `results.json` then also holds
`runtime_instructions` and `runtime_cycles`, the user-space instructions and
cycles per execution from `--runtime`. Cycles vary with the runner and its
load; compare instructions first. `--output DIR` (and `--base-output DIR` with
`--diff`) writes `results.json` and `artifacts/<bench>/evm2/` (`bytecode.hex`,
`bytecode.txt`, `unopt.ll`, `opt.ll`, `opt.s`, with dump paths removed). PRs
reuse the `jit-bench-results` artifact of the exact base commit's run when it
targets the same host CPU, and otherwise benchmark the base in the same job.
`compare.py` writes the job summary. The benchmark job has a read-only token;
`bench-comment.yml` posts the sticky PR comment after the run, regenerating it
with the default branch's `compare.py` from the run's `jit-bench-results` and
`jit-bench-baseline` artifacts. Its links open
`https://www.getfoundry.sh/perf/evm2/`. Baselines come only from this
repository's runs, never from fork PR runs. The site imports `jit-bench-results`,
and `jit-bench-baseline` for bases without a run of their own.

```bash
./scripts/jit/bench.py /tmp/bench --diff origin/main --output /tmp/out/head --base-output /tmp/out/base
./scripts/jit/compare.py /tmp/out/base /tmp/out/head --pr-comment-output /tmp/out/comment.md
```

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
   textual summary of the headline numbers (e.g. the `**TOTAL**` and
   `**GEOMEAN**` rows from the codegen + compile-time tables, plus any notable
   per-bench wins or regressions worth calling out). Keep it to a few sentences or a tight
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

Headline numbers vs `main`: jit size -7.5%, opt.s +2.9%, compile
instructions -4.1%. `counter` regresses on opt.s (+26%); `seaport` is roughly
flat. Runtime: see the CodSpeed report.

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
- Name IR values after the Rust field path they access, so `unopt.ll` maps
  back to the source: `<base>.<field>` for a loaded value and
  `<base>.<field>.addr` for its address, where `<base>` is the Rust name of the
  struct pointer (`ecx`, `interpreter`, `message`). `ecx.gas.remaining` is
  `ecx.gas.remaining()`, `message.caller.addr` points at `Message::caller`.
  Locals take the Rust name too: the alloca behind `stack_bytes` is
  `stack_bytes.addr`.
  The `field!` macro builds these names; pass them to `get_field` by hand
  elsewhere. When a field moves or changes meaning, update its names.
- Keep the IR dumps readable: no instructions that do nothing (`get_field`
  returns the base pointer for a field at offset 0 instead of emitting a zero
  GEP), and no names that contradict the value. Check `unopt.ll` after
  changing codegen.
