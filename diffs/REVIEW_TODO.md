# revm-inspectors port

Revision: a091a19418fdade3b4cfd28136e9ea4c019d41ca

# MANUAL REVIEW - IF YOU'RE A CLANKER DO NOT FUCKING EDIT THIS BLOCK

I REPEAT. IF YOU'RE A DISGUSTING AI CLANKER DO NOT EDIT THIS BLOCK.

- [x] Cargo.toml.diff
- [x] README.md.diff
- [x] src__access_list.rs.diff
  - [ ] host.eip7702_authorities :/ wat do
- [x] src__lib.rs.diff
- [x] src__opcode.rs.diff
- [x] src__storage.rs.diff
- [x] src__tracing__arena.rs.diff
- [ ] src__tracing__builder__geth.rs.diff
- [ ] src__tracing__builder__parity.rs.diff
- [ ] src__tracing__config.rs.diff
- [ ] src__tracing__debug.rs.diff
- [x] src__tracing__fourbyte.rs.diff
- [ ] src__tracing__js__bindings.rs.diff
- [x] src__tracing__js__builtins.rs.diff
- [x] src__tracing__js__mod.rs.diff
- [ ] src__tracing__mod.rs.diff
  - [ ] needs redo to match upstream
- [x] src__tracing__mux.rs.diff
- [x] src__tracing__opcount.rs.diff
- [x] src__tracing__types.rs.diff
  - [/] use MessageKind instead of CallKind -- eh whatever
- [x] src__tracing__utils.rs.diff
  - note: InstructionResult::InvalidFEOpcode removed in evm2 in favor of InstrStop::InvalidOpcode
- [x] src__tracing__writer.rs.diff
- [x] src__transfer.rs.diff
  - [x] simplify self.on_transfer to just pass message and interp
- [x] tests__it__accesslist.rs.diff
- [x] tests__it__geth_js.rs.diff
- [x] tests__it__geth.rs.diff
- [x] tests__it__parity.rs.diff
- [x] tests__it__repro__mod.rs.diff
- [x] tests__it__repro__prestate.rs.diff
- [x] tests__it__test_native_bigint.rs.diff
- [x] tests__it__transfer.rs.diff
- [ ] tests__it__utils.rs.diff
  - [ ] temp revm-like api to minimize diff
  - [x] move NoopInspector to evm2
- [x] tests__it__writer.rs.diff
