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

Execution preparation recognizes PUSH1 through PUSH32, the two-byte operands of RJUMP and RJUMPI, and the one-byte operands of DUPN, SWAPN and EXCHANGE; the original jump map follows the existing PUSH-only analysis. The execution view replaces leading continuation bytes with `JUMPDEST` and marks every replacement position as a valid target. This is an explicit semantic change: JUMP and taken JUMPI may enter these former immediate bytes. Ordinary immediate data remains invalid. Normal fallthrough skips the replacement prefix.

A backward scan computes the required continuation length for every entry position once. Up to 33 shared execution variants are cached by that length. Each taken jump validates the full original jump map, then selects the variant for its target. This preserves valid targets even where the existing PUSH-only map and execution operand parsing disagree.

The execution buffer appends the missing immediate bytes and a three-byte generated `RJUMP -3` tail, or final STOP. Its size is at most 24,576 bytes. RJUMP uses no stack slots and costs 2 gas, including generated transfers. It may continue at a non-JUMPDEST instruction. Overlap and tails have no valid user jump bits. Dispatch executes the ordinary RJUMP handler with no generated-tail special case or chunk-boundary check. Inspectors use the authenticated tail position only to hide internal instructions.

`RJUMP` uses opcode `0xe0`, following EOF's name, with a TIP-1143-specific operand encoding. Each of its two immediate bytes uses the DUPN/SWAPN alphabet: `00..5a` and `80..ff`. Decode each with EIP-8024's `decode_single`, subtract 17, and combine the two digits as `high * 219 + low`. Values above 23980 represent negative offsets by subtracting 47961. This gives a signed range of -23980 through +23980 and keeps PUSHn and JUMPDEST bytes out of the immediate. The offset is relative to the logical PC after the three-byte instruction; targets must be inside the original code. This draft permits any in-range byte target and does not adopt EOF's container validation. The opcode is enabled only with TIP-1143, including resident initcode; it remains unknown otherwise.

The generated tail is `e0 5a 58` (offset -3). Its logical PC already points at the next original instruction, including any copied immediate bytes. Subtracting 3 from the post-instruction PC selects that same logical offset in the successor chunk. The handler reserves the usual cold/warm chunk tariff before fetching a remote target, while same-chunk targets need no chunk access charge.

`CodeChunk::bytecode(multi_chunk)` returns a shared canonical prepared analysis after state loading. The crate-private `execution_view(entry)` selects another entry layout. Its jump map admits only destinations with the same continuation length. If entry zero needs another layout, this variant replaces byte zero with STOP so its Bytecode remains safe to execute independently from zero; runtime entry zero always selects the canonical variant. This guard is invisible to original-byte copies. Its bytecode bytes are an execution view; use `CodeChunk::original_bytes()` for original data. Do not publish the prepared Bytecode as the account's whole contract. Active runtime frames carry the separate original size, global code hash and chunk index.

## Account types and delegation

`AccountInfo::extension` is the only source of account code type. It is always available, including in builds without default features; there is no separate `account-ext` feature or opaque account payload.

- Empty extension: ordinary code, with emptiness determined by `code_hash`.
- Version 1: `AccountExtension::chunked(CodeMetadata)` contains original code size and ordered chunk hashes. Metadata validates lengths 24,542 through 981,640 and exactly `ceil(size / 24541)` hashes.
- Version 2: `AccountExtension::delegated(Address)` contains the delegation target. The ordinary account code hash commits to its marker.

`AccountExtension::encode` emits empty bytes, `1 || code_size_be_u32 || chunk_hashes`, or `2 || target_20_bytes`. `decode` rejects unknown versions, invalid sizes/counts, malformed lengths and zero delegation targets. Serde uses this byte encoding and omits empty extensions from account records. Client account/trie codecs must preserve the selected type and commitments.

`AccountInfo::code_metadata()` and `delegation_target()` read the extension. `inline_delegation_code()` validates the target and marker hash, then synthesizes the marker without payload I/O or a chunk tariff. Providers must normalize historical delegation records into version 2 metadata when loading accounts. There is no execution-time kind lookup or fallback to a cached bytecode kind. Only one delegation hop is followed.

`CodeChunk::from_bytecode` preserves existing analysis and kind. `CodeChunk::new` is unclassified and treats payloads as ordinary instructions, including marker-shaped bytes. `AccountHandle::set_chunked_code` explicitly selects version 1 when needed; `set_code_slow` does not infer chunking from size. Installing delegation code selects version 2. Replacing or clearing code replaces the extension.

Code setters, `set_extension`, and `set_info` invalidate account-local chunks immediately when code identity changes. No public account mutation method exposes an unrestricted mutable `AccountInfo`. Handle drop only journals its snapshot; rollback restores the account, cached chunks and warmth together.

## Provider and cache contract

Typed, erased, counted and asynchronous database interfaces require `get_code_chunk_by_hash(hash, index) -> Result<Option<CodeChunk>, Error>`. Code type comes from `get_account`; no separate kind lookup is required. There is no default multi-chunk full-code fallback. A chunked request needs only the selected original slice and its bounded preparation. Providers authenticate original identity and preparation at ingestion; execution validates context without rehashing every payload.

Legacy account execution uses the explicit `get_code_by_hash` record operation after reserving its chunk-zero gas. This preserves an unchanged 24,576-byte legacy record even if another account with the same hash uses the new stride. The selected account representation, not descriptor presence or payload length, chooses the operation. Chunked requests must not use a cached full original record to hide missing prepared data.

`Cache::insert_code` records original code without automatically splitting historical records. Account insertion publishes prepared chunks only when account metadata selects that representation. Creation prepares its chunks before publication. Original and prepared caches retain immutable data across transaction reset and revert.

Prepared entries are keyed by `(global_code_hash, index)`. Each `CodeChunk` owns shared lazy analysis and prepared execution views; no separate analyzed-chunk map is needed. Identical slice hashes are insufficient: leading replacement counts and continuation bytes depend on neighboring instructions. Returned CodeChunk values retain the shared analysis so repeated frame activation does not analyze again.

`State::load_code_chunk` returns a `CodeChunkHandle` after checking bounds, skipped cold accesses, payload length and preparation context. Loading does not change warmth or record a journal entry. The handle exposes `get()` and `is_warm()`; `warm()` marks access and journals the cold-to-warm transition, returning whether the chunk was cold. Rollback restores warmth while retaining cached bytes, as it does for storage reads. The host reserves all known chunk tariffs before loading, then explicitly warms the handle. Missing required data and corruption become node errors, never empty bytecode or EVM halts. Rejected entries are evicted through typed, cached, counted and async adapters. Physical residency never determines consensus warmth or price.

Account warmth is keyed by owner and index. Journals restore warmth and code identity at rollback; transaction clearing resets logical warmth while retaining immutable bytes.

## Execution and client handoff

`LoadedCode` and `MessageExt` carry one `code_chunk` container. `MessageExt::is_lazy_code` explicitly selects lazy runtime loading and chunk tariffs. It is false for resident initcode, synthesized delegation markers and pre-TIP frames. The interpreter obtains its active bytecode view from the chunk.

Both explicit jump opcodes use unchanged global positions. Untaken JUMPI does not read or warm its target. Cross-chunk jumps and internal fallthrough reserve destination gas before I/O. Copy operations use original bytes and reserve all intersecting chunk costs before the first load. Inline marker copies are synthesized from account metadata. Legacy EXTCODECOPY keeps its 24,576-byte source threshold, independent of the new runtime stride.

Draft frames, including resident initcode, run through the interpreter; JIT/AOT support is deferred. Inspectors must report original PCs and omit internal tails. Creation limits apply through transaction, embedding, CREATE and CREATE2 paths, while ordinary gas, memory and deposit constraints remain.

Reth/Tempo must authenticate ingestion, atomically persist original payloads plus context preparation and account type, preserve historical state views, reconstruct original RPC bytes, and use one coherent immutable dependency graph. Production inventory, replay, proof growth, provider latency, prepared-buffer costs and final tariff decisions remain activation prerequisites.


`RJUMPI` (`0xe1`) uses the same two-byte offset encoding and costs 4 gas. It pops one condition: nonzero jumps through the shared RJUMP target loader, while zero advances past the immediate. An untaken branch does not validate, load, charge or warm its target chunk; ordinary fallthrough can still cross a chunk boundary and execute a generated RJUMP. Invalid immediate encodings fail even for an untaken branch. Both relative opcodes are unknown when TIP-1143 is disabled.
