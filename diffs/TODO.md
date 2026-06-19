# revmc port

Revision: b5b60c1491c4cda92626e01e8e838bf29e53736a

Local port: `crates/jit`

# MANUAL REVIEW - DO NOT EDIT WITHOUT USER REAL HUMAN CONSENT

I REPEAT. IF YOU ARE A DISGUSTING AI CLANKER, DO NOT MODIFY THIS LIST.

Only the user, a real human, may edit this checklist, or explicitly tell an agent to regenerate it.

Generated from non-empty unified diffs in `diffs/` by `./scripts/generate_jit_diffs.sh --write-todo`.

- [ ] README.md.diff
- [x] crates__revmc-backend__Cargo.toml.diff
- [x] crates__revmc-backend__README.md.diff
- [x] crates__revmc-backend__src__traits.rs.diff
- [x] crates__revmc-build__Cargo.toml.diff
- [x] crates__revmc-build__README.md.diff
- [x] crates__revmc-build__src__lib.rs.diff
- [x] crates__revmc-builtins__Cargo.toml.diff
- [x] crates__revmc-builtins__README.md.diff
- [x] crates__revmc-builtins__src__gas.rs.diff
  - re-use gas constants
- [x] crates__revmc-builtins__src__ir.rs.diff
- [ ] crates__revmc-builtins__src__lib.rs.diff
- [x] crates__revmc-builtins__src__macros.rs.diff
- [ ] crates__revmc-builtins__src__utils.rs.diff
- [x] crates__revmc-codegen__Cargo.toml.diff
- [x] crates__revmc-codegen__README.md.diff
- [x] crates__revmc-codegen__build.rs.diff
- [x] crates__revmc-codegen__src__bytecode__asm.rs.diff
- [x] crates__revmc-codegen__src__bytecode__fmt.rs.diff
- [x] crates__revmc-codegen__src__bytecode__info.rs.diff
- [x] crates__revmc-codegen__src__bytecode__mod.rs.diff
- [x] crates__revmc-codegen__src__bytecode__opcode.rs.diff
- [x] crates__revmc-codegen__src__bytecode__passes__block_analysis.rs.diff
- [x] crates__revmc-codegen__src__bytecode__passes__const_fold.rs.diff
- [x] crates__revmc-codegen__src__bytecode__passes__dead_store_elim.rs.diff
- [x] crates__revmc-codegen__src__bytecode__passes__dedup.rs.diff
- [x] crates__revmc-codegen__src__bytecode__passes__memory_sections.rs.diff
- [x] crates__revmc-codegen__src__bytecode__passes__sections.rs.diff
- [x] crates__revmc-codegen__src__compiler__mod.rs.diff
- [ ] crates__revmc-codegen__src__compiler__translate__mod.rs.diff
  - suspend removal good
- [x] crates__revmc-codegen__src__compiler__translate__peephole.rs.diff
- [x] crates__revmc-codegen__src__compiler__translate__vstack.rs.diff
- [x] crates__revmc-codegen__src__lib.rs.diff
- [x] crates__revmc-codegen__src__linker.rs.diff
- [x] crates__revmc-codegen__src__tests__fibonacci.rs.diff
- [x] crates__revmc-codegen__src__tests__macros.rs.diff
- [x] crates__revmc-codegen__src__tests__meta.rs.diff
- [ ] crates__revmc-codegen__src__tests__mod.rs.diff
- [ ] crates__revmc-codegen__src__tests__runner.rs.diff
- [x] crates__revmc-context__Cargo.toml.diff
- [x] crates__revmc-context__README.md.diff
- [x] crates__revmc-context__src__arch__aarch64.rs.diff
- [x] crates__revmc-context__src__arch__mod.rs.diff
- [x] crates__revmc-context__src__arch__x86_64.rs.diff
- [ ] crates__revmc-context__src__lib.rs.diff
- [x] crates__revmc-llvm__Cargo.toml.diff
- [x] crates__revmc-llvm__README.md.diff
- [x] crates__revmc-llvm__build.rs.diff
- [x] crates__revmc-llvm__cpp__lib.cpp.diff
- [x] crates__revmc-llvm__src__cpp.rs.diff
- [x] crates__revmc-llvm__src__lib.rs.diff
- [x] crates__revmc-llvm__src__orc.rs.diff
- [x] crates__revmc-llvm__src__utils.rs.diff
- [x] crates__revmc-runtime__Cargo.toml.diff
- [x] crates__revmc-runtime__README.md.diff
- [x] crates__revmc-runtime__src__lib.rs.diff
- [ ] crates__revmc-runtime__src__revm_evm.rs.diff
- [x] crates__revmc-runtime__src__runtime__api.rs.diff
- [x] crates__revmc-runtime__src__runtime__backend.rs.diff
- [x] crates__revmc-runtime__src__runtime__config.rs.diff
- [x] crates__revmc-runtime__src__runtime__mod.rs.diff
- [x] crates__revmc-runtime__src__runtime__out_of_process.rs.diff
- [x] crates__revmc-runtime__src__runtime__stats.rs.diff
- [x] crates__revmc-runtime__src__runtime__storage.rs.diff
- [x] crates__revmc-runtime__src__runtime__tests.rs.diff
- [x] crates__revmc-runtime__src__runtime__worker.rs.diff
- [x] crates__revmc__Cargo.toml.diff
- [x] crates__revmc__build.rs.diff
- [x] crates__revmc__src__lib.rs.diff
- [x] docs__out-of-process-jit.md.diff
- [ ] examples__compiler__Cargo.toml.diff
- [ ] examples__compiler__build.rs.diff
- [ ] examples__compiler__src__main.rs.diff
- [x] scripts__bench.py.diff
- [x] scripts__utils.py.diff
- [ ] tests__codegen__address_mask_coinbase.evm.diff
- [ ] tests__codegen__address_mask_origin.evm.diff
- [ ] tests__codegen__dead_push_pop.evm.diff
- [ ] tests__codegen__memory__base_cache_after_builtin.evm.diff
- [ ] tests__codegen__memory__base_cache_conditional.evm.diff
- [ ] tests__codegen__memory__base_cache_dynamic.evm.diff
- [ ] tests__codegen__memory__base_cache_keccak.evm.diff
- [ ] tests__codegen__memory__branch_join.evm.diff
- [ ] tests__codegen__memory__branch_join_missing.evm.diff
- [ ] tests__codegen__memory__call.evm.diff
- [ ] tests__codegen__memory__call_dynamic_output.evm.diff
- [ ] tests__codegen__memory__const_roundtrip.evm.diff
- [ ] tests__codegen__memory__copy_hash_log.evm.diff
- [ ] tests__codegen__memory__create.evm.diff
- [ ] tests__codegen__memory__direct_grow.evm.diff
- [ ] tests__codegen__memory__dynamic_offset.evm.diff
- [ ] tests__codegen__memory__failure.evm.diff
- [ ] tests__codegen__memory__mcopy.evm.diff
- [ ] tests__codegen__memory__mload_mstore.evm.diff
- [ ] tests__codegen__memory__mresize.evm.diff
- [ ] tests__codegen__memory__mstore8.evm.diff
- [ ] tests__codegen__memory__return.evm.diff
- [ ] tests__codegen__memory__revert.evm.diff
- [ ] tests__codegen__memory__solidity_prologue.evm.diff
- [ ] tests__codegen__memory__zero_len_dynamic_offset.evm.diff
