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
- Storing the value last stored to a stack slot where codegen reloads the slot
  only to copy it to another slot, which LLVM does with a 256-bit vector load
  that stalls store forwarding from the four 64-bit stores: spills +57%,
  reloads +32%, JIT size +2.2% for 0.7% fewer runtime instructions. Using the
  stored value for every read of the slot also adds instructions; both save
  cycles in some Solidity loops and cost them in others. The `noimplicitfloat`
  function attribute, which keeps those copies in general-purpose registers,
  made a Solady string loop 13–50% slower in cycles.
- Dropping `relieve_vstack_pressure`, so the virtual stack only stores at
  section boundaries and builtin calls: i256 stores -23%, but JIT size +13%,
  spills ×6 and runtime instructions +9.6% (onchain_lm_v2 +13.5%).

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

- `separate-const-offset-from-gep` at the end: spills -12% and reloads -6% on
  the corpus, but JIT size grows on 33 of 132 contracts and runtime is flat.
  `memcpyopt`, which merges adjacent zero stack stores into a `memset`: JIT
  size -0.2% for +0.3% opt instructions, runtime flat.
- `gvn-sink`: JIT size -0.8% but spills +9%. `gvn-hoist` costs 85% more opt
  time, `load-store-vectorizer` grows JIT size by 6%, `loop(indvars)` raises
  reloads by 13%, and `-jump-threading-across-loop-headers` by 3%.
- `div-rem-pairs`, `speculative-execution`, `infer-alignment`,
  `vector-combine`, `callsite-splitting`, `dce`, `lower-constant-intrinsics`,
  `sroa<preserve-cfg>`, `simplifycfg<switch-to-lookup>`, the SimplifyCFG phi
  folding thresholds, and the DSE partial-store options change nothing;
  `slp-vectorizer`, `slsr`, and `nary-reassociate` change at most 8 contracts
  for 3–16% more opt time. `mem2reg` in place of `sroa` produces the same code
  for 1% fewer opt instructions.

## `llc` flags

- `-grow-region-complexity-budget=1000` raised snailtracer reloads 2.2×, and
  `-disable-machine-licm` changed 12 of 21 benchmarks with seaport JIT size
  +1.6%; `-sink-insts-to-avoid-spills` fixes the function-wide loops better.
- `-x86-cmov-converter=false` churns (big18 JIT size +2%, extra_663c2110
  -4.9%); `-enable-misched=false` raises reloads by 15%;
  `-misched-regpressure=false` raises spills by 4–9%;
  `-enable-tail-merge=false` grows code by 3.4% and
  `-combiner-store-merging=false` by 1%; `-pre-RA-sched=source` changes
  nothing.
- `-disable-lsr`: reloads -10% on the corpus, but fibonacci-calldata JIT size
  +8% and runtime +3.9%. On the function-wide loops LSR mostly adds
  loop-simplify blocks and LCSSA phis. `-lsr-drop-solution`,
  `-lsr-insns-cost=false`, `-lsr-exp-narrow`, and the LSR limits change little.
- `-disable-constant-hoisting`: ConstantHoisting keeps the saturated memory
  offset 2^63-1, which needs `movabs`, in a register across much of the
  function. Turning it off saves 0.8% of llc instructions and 0.1% of JIT
  size, but reloads move both ways: seaport +25%, hot47 +103%, hot12 -69%.
- `-jump-is-expensive` merges the gas and stack checks into one branch through
  `setcc`/`or`: reloads -18% and spills -10% on the corpus, but burntpix and
  curve_stableswap runtime +1.6–1.7% in every run.
- `-enable-machine-outliner`: JIT size -11.5%, runtime +1.2% geomean and
  onchain_lm_v2 +8.4%.
- `-fast-isel` grows JIT size by 4.5%, `-global-isel` falls back to
  SelectionDAG, and `-regalloc=basic` grows JIT size by 19%.
- Codegen level, with the IR pipeline fixed: `-O1` gives the same code as
  `-post-RA-scheduler=false` and is what `Default` uses. Against `-O3` it cuts
  compile instructions by 5.5% and spills by 0.4% for 0.3% more JIT size and
  flat runtime (instructions -0.1%, cycles +1.6% on a loaded machine, then
  +0.1% and -0.1%), but the JIT size of push0_proxy grows 6.4%. `-O2` and `-O3` produce the same
  code to 0.1% in JIT size and compile instructions, so the post-RA scheduler
  is the whole difference and `-O3` buys nothing. `-O0` grows JIT size by 71%,
  spills ×27, and runtime instructions by 16%, for 41% fewer compile
  instructions.
- `-enable-ext-tsp-block-placement`: JIT size -0.3% for 56% more llc time.
  `-combiner-global-alias-analysis`: llc +3.4% for no smaller code.
- Turning off `machine-cse`, `branch-fold`, block placement, `machine-cp`, the
  scheduling model, or post-RA machine sinking saves up to 1.7% of llc
  instructions but grows code by up to 0.6%; turning off `machine-sink`,
  `peephole`, stack slot coloring, or early tail duplication costs both, and
  turning off CodeGenPrepare nearly doubles spills.
  `-x86-machine-combiner=false` raises reloads by 8%.
- Flags that move spills and reloads both ways across contracts with little
  change in JIT size: `-split-spill-mode=size`,
  `-twoaddr-reschedule=false`, `-misched-prera-direction=bidirectional`,
  `-combiner-topological-sorting`, `-min-jump-table-entries=8` (JIT size
  -1.4% from fewer jump tables, reloads +0.9%), `-tail-dup-size`,
  `-tail-merge-size=1`, `-tail-dup-placement=false`,
  `-machine-sink-split=false`, `-x86-br-merging-base-cost`, and the greedy
  eviction and hint-split limits.
- No change on any contract: `-disable-early-ifcvt`, `-x86-early-ifcvt`,
  `-disable-select-optimize`, `-disable-complex-addr-modes`,
  `-disable-cgp-branch-opts`, `-enable-local-reassign`,
  `-enable-split-loopiv-heuristic`, `-terminal-rule`, `-join-splitedges`,
  `-join-globalcopies`, `-enable-shrink-wrap=false`, `-enable-post-misched`,
  `-misched-cluster=false`, and the CodeGenPrepare type-promotion and
  address-sinking options.
