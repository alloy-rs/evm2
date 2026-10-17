# Rejected JIT experiments

Experiments that did not pay off, so you don't repeat them blindly. Add new
ones here with the numbers that rejected them.

## Codegen

- Keeping the remaining gas in a local (`alloca`) synced with `ecx.gas` around
  calls: x86 turns `sub qword ptr [ecx], imm; jb` into `cmp`/`lea` on a
  register that stays live everywhere, so spills went up 72%.
- `preserve_most` stubs for host builtins (`SLOAD`, `CALL`, ...): fewer spills
  in some contracts, many more reloads in others, about neutral overall.
- Folding the memory offset range check into the resize branch: LLVM merges
  the conditions back into the same compare chain and the cold path grows.
- A never-taken entry edge into the dynamic jump table, which makes the
  function-wide loops irreducible: misses loops closed by internal-function
  return switches and raised compile cost by up to 50% on some contracts.
  `-sink-insts-to-avoid-spills` handles these loops instead.

## Optimization passes

- `early-cse<memssa>`: forwards stack loads across blocks but raises register
  pressure; erc20_transfer spills went from 12 to 87.
- More load forwarding of any kind: `gvn` (opt time ×67 on the mainnet corpus,
  spills ×3), `newgvn` (spills +78–94%, JIT size +2.7–4.1%), and
  `-available-load-scan-limit=24`, which forwards within blocks in
  `instcombine` and `jump-threading` (JIT size +1.0–1.6%). Reloading a stack
  slot after a call or store is cheaper than keeping an i256 in four registers
  across it.
- Dropping passes whose codegen effect looks neutral: the `simplifycfg` before
  the first `instcombine`, the `simplifycfg` and `instcombine` after LICM, and
  the last `instcombine` cut opt instructions by 19% at flat JIT size, but
  runtime instructions moved with register allocation: burntpix +1.4% to
  +5.8%, onchain_lm_v2 +4.1% without the last `instcombine`.
- `constraint-elimination`: JIT size -0.1% to -0.3% for 20–32% more opt time.
- LICM at every function size: opt instructions +75% on the mainnet corpus,
  JIT size +0.4%, reloads +12%. No LICM at all: JIT size -0.1%, but hash_10k
  runtime +1.2%.
- `bdce`, `aggressive-instcombine`, `mldst-motion`, `dfa-jump-threading`, and
  `simplifycfg` with `switch-range-to-icmp`, `switch-to-arithmetic`,
  `forward-switch-cond` or `no-keep-loops` change at most one benchmark.
  `sink`, `reassociate`, a second `jump-threading` and
  `correlated-propagation`, and
  `simplifycfg<hoist-common-insts;sink-common-insts>` move JIT size by less
  than 0.5% for more opt time.
- DSE takes a quarter of opt time, but dropping it grows JIT size by 1%;
  `-dse-memoryssa-walklimit=30` saves 6% of opt instructions for +0.05% size.

## `llc` flags

- `-jump-table-density=1` for the dynamic jump switch: about 5% less code, but
  the tables grow by about 1 MB across the benchmarks because the switch is on
  sparse PCs. It needs a dense index first.
- `-grow-region-complexity-budget=1000` raised snailtracer reloads 2.2×, and
  `-disable-machine-licm` changed 12 of 21 benchmarks with seaport JIT size
  +1.6%; `-sink-insts-to-avoid-spills` fixes the function-wide loops better.
- `-x86-cmov-converter=false` churns (big18 JIT size +2%, extra_663c2110
  -4.9%); `-enable-misched=false` raises reloads by 15%;
  `-misched-regpressure=false` raises spills by 4–9%;
  `-enable-tail-merge=false` grows code by 3.4% and
  `-combiner-store-merging=false` by 1%; `-pre-RA-sched=source` changes
  nothing.
