# TIP-1143 EVM2 integration contract

This is an opt-in, unscheduled draft. It does not select an activation date or certify production replay, storage compatibility, or gas calibration. Both the default Ethereum authorization policy and Tempo's authorization hook remain available.

## Original bytes and prepared execution

`Version::with_tip1143(bool)` controls `EvmFeatures::TIP1143`. Original runtime slices contain 24,541 bytes, up to 40 slices and 981,640 bytes in total. Initcode remains resident with a separate 1,966,080-byte limit. Chunk tariffs are provisionally 28,680 cold and 1,000 warm gas. Disabling the draft restores the previous limits.

Original code determines global hashes, slice hashes, code size, deposit gas, copies and RPC output. Creation scans the complete original output before publishing account metadata and prepared execution buffers. It accepts crossing and truncated PUSH immediates; source code needs no boundary STOP. `code_metadata` computes original commitments. `code_chunk(&Bytes, index)` derives a selected slice and its bounded preparation from authenticated full original code.

A `CodeChunk` contains original bytes separately from its execution view. `CodeChunk::with_preparation(bytes, code_size, index, leading_data_len, jump_data_len, lookahead)` restores provider preparation and checks its local bounds and instruction layout. `validate_context(code_size, index)` checks its account context. Neither function authenticates byte values against original full code: ingestion must do that before publication.

`prepared()` exposes `PreparedCodeChunk`:

- `leading_data_len()` counts original immediate bytes copied into the preceding chunk.
- `jump_data_len()` carries the independent original PUSH-only jump-analysis prefix, preserving the existing treatment of DUPN/SWAPN/EXCHANGE operands.
- `lookahead()` contains exactly the next `min(32, remaining original bytes)` bytes. Per-entry execution supplies any required final zero padding.
- `continuation()` describes the canonical entry path; alternative valid entry paths can need a different continuation length.
- `next_chunk()` selects an internal transfer, or final STOP.
- `tail_offset(payload_len)` identifies the authenticated generated tail.

Execution preparation recognizes PUSH1 through PUSH32 and the one-byte operands of DUPN, SWAPN and EXCHANGE; the original jump map follows the existing PUSH-only analysis. The execution view replaces leading continuation bytes with `JUMPDEST` and marks every replacement position as a valid target. This is an explicit semantic change: JUMP and taken JUMPI may enter these former immediate bytes. Ordinary immediate data remains invalid. Normal fallthrough skips the replacement prefix.

A backward scan computes the required continuation length for every entry position once. Up to 33 shared execution variants are cached by that length. Each taken jump validates the full original jump map, then selects the variant for its target. This preserves valid targets even where the existing PUSH-only map and execution operand parsing disagree.

The execution buffer appends the missing immediate bytes and a three-byte generated `PUSH1 / chunk index / JUMP` tail, or final STOP. Its size is at most 24,576 bytes. The generated transfer is fused machinery: it does not use the contract stack or charge PUSH/JUMP opcode gas. It may continue at a non-JUMPDEST instruction. Overlap and tails have no valid user jump bits. Runtime must recognize the tail only from the active authenticated layout. The main dispatch loop has no chunk-boundary check.

`CodeChunk::bytecode(multi_chunk)` returns a shared canonical prepared analysis after state loading. The crate-private `execution_view(entry)` selects another entry layout. Its jump map admits only destinations with the same continuation length. If entry zero needs another layout, this variant replaces byte zero with STOP so its Bytecode remains safe to execute independently from zero; runtime entry zero always selects the canonical variant. This guard is invisible to original-byte copies. Its bytecode bytes are an execution view; use `CodeChunk::original_bytes()` for original data. Do not publish the prepared Bytecode as the account's whole contract. Active runtime frames carry the separate original size, global code hash and chunk index.

## Account types and delegation

`AccountInfo::code_metadata: Option<CodeMetadata>` identifies chunked runtime. Metadata validates lengths 24,542 through 981,640 and exactly `ceil(size / 24541)` hashes. Empty and unchanged legacy records have no chunk metadata. New code of at most one original slice also retains the legacy representation.

`AccountInfo::inline_delegation: Option<Address>` identifies an account-owned delegation marker. `inline_delegation_code()` validates the account hash, nonzero target and absence of runtime metadata, then synthesizes the exact marker without payload I/O. The draft authorization path publishes this variant atomically. Clearing or replacing code clears obsolete delegation and runtime metadata. Unrelated account extension bytes are retained.

Historical delegation uses `get_code_kind_by_hash` to read the stored kind without loading ordinary code. Only a known delegation kind permits loading the bounded 23-byte marker, whose length and account hash are checked. CacheDB retains both positive and negative kind evidence, as well as a loaded marker, so repeated recognition requires no further provider lookup. A known historical marker Bytecode may also be supplied at account loading. Runtime resolves these markers without a marker chunk tariff. This compatibility path is distinct from persisted inline delegation; it does not migrate old accounts. New inline accounts require no marker lookup. Only one delegation hop is followed.

`CodeChunk::from_bytecode` preserves a persisted legacy/delegation kind. `CodeChunk::new` is unclassified and always treated as legacy instructions. Marker-like prefixes never convert an unclassified payload into delegation. Explicit `AccountHandle::set_chunked_code` validates and selects the draft runtime representation; ordinary `set_code_slow` does not infer representation from size.

Account equality and rollback include both typed code fields. Serde retains the old positional account shape when both are absent. Client adapters must preserve the fields through their actual account, bundle, trie and storage paths. An arbitrary opaque account-extension prefix is not an unambiguous type discriminator. The EVM2 fields alone do not solve that cross-client persistence requirement.

## Provider and cache contract

Typed, erased, counted and asynchronous database interfaces require `get_code_kind_by_hash(hash) -> Result<BytecodeKind, Error>` and `get_code_chunk_by_hash(hash, index) -> Result<Option<CodeChunk>, Error>`. The kind operation must read bounded type metadata, not fetch a whole ordinary code record. There is no default multi-chunk full-code fallback. A chunked request needs only the selected original slice and its bounded preparation. Providers authenticate original identity and preparation at ingestion; execution validates context without rehashing every payload.

Legacy account execution uses the explicit `get_code_by_hash` record operation after reserving its chunk-zero gas. This preserves an unchanged 24,576-byte legacy record even if another account with the same hash uses the new stride. The selected account representation, not descriptor presence or payload length, chooses the operation. Chunked requests must not use a cached full original record to hide missing prepared data.

`Cache::insert_code` records original code without automatically splitting historical records. Account insertion publishes prepared chunks only when account metadata selects that representation. Creation prepares its chunks before publication. Original and prepared caches retain immutable data across transaction reset and revert.

Raw prepared entries are keyed by `(global_code_hash, index)`. Analyzed entries additionally distinguish legacy and chunked representation. Identical slice hashes are insufficient: leading replacement counts and continuation bytes depend on neighboring instructions. Returned CodeChunk values retain the shared analysis so repeated frame activation does not analyze again.

`State::load_code_chunk` checks logical warmth, bounds, payload length and preparation context. Its caller must reserve all known chunk tariffs before invoking it. Missing required data and corruption become node errors, never empty bytecode or EVM halts. Rejected entries are evicted through typed, cached, counted and async adapters. Physical residency never determines consensus warmth or price.

Account warmth is keyed by owner and index. Journals restore warmth and code identity at rollback; transaction clearing resets logical warmth while retaining immutable bytes. Aggregate diagnostics track logical accesses, physical hits/misses, fetched original bytes, analysis counts and latency without address/hash labels. They are observations, not gas inputs.

## Execution and client handoff

Both explicit jump opcodes use unchanged global positions. Untaken JUMPI does not read or warm its target. Cross-chunk jumps and internal fallthrough reserve destination gas before I/O. Copy operations use original bytes and reserve all intersecting chunk costs before the first load. Inline marker copies are synthesized from account metadata. Legacy EXTCODECOPY keeps its 24,576-byte source threshold, independent of the new runtime stride.

Draft frames, including resident initcode, run through the interpreter; JIT/AOT support is deferred. Inspectors must report original PCs and omit internal tails. Creation limits apply through transaction, embedding, CREATE and CREATE2 paths, while ordinary gas, memory and deposit constraints remain.

Reth/Tempo must authenticate ingestion, atomically persist original payloads plus context preparation and account type, preserve historical state views, reconstruct original RPC bytes, and use one coherent immutable dependency graph. Production inventory, replay, proof growth, provider latency, prepared-buffer costs and final tariff decisions remain activation prerequisites.

