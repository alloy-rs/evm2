# TIP-1143 EVM2 integration contract

This is an opt-in, unscheduled draft. It does not select an activation date or certify production replay, storage compatibility, or gas calibration. Both the default Ethereum authorization policy and Tempo's authorization hook remain available.

## Logical code and stored chunks

`Version::with_tip1143(bool)` controls `EvmFeatures::TIP1143`. Logical runtime slices contain at most 24,540 bytes (`24 KiB - 36`), up to 40 slices and 981,600 bytes in total. Initcode remains resident with a separate 1,966,080-byte limit. Chunk tariffs are provisionally 28,680 cold and 1,000 warm gas. Disabling the draft restores the previous limits.

Each prepared chunk has this stored format:

```text
| original code: at most 24,540 bytes | padding: exactly 32 bytes | STOP or RJUMP: 1 or 3 bytes | start offset: 1 byte |
```

A full chunk with a transfer occupies exactly 24 KiB. The final chunk may contain less original code. Ordinary legacy records are unchanged and have no trailer. Bytecode adds its usual in-memory safety padding separately; that safety padding is not stored or hashed.

The complete logical code determines the account hash, code size, deposit gas, copy results and RPC output. Ordered chunk hashes commit to the complete stored buffers, including padding, generated tails and the offset byte. Creation or ingestion prepares the buffers before publication; `code_metadata` computes their commitments and `code_chunk(&Bytes, index)` derives one buffer from complete logical code.

`CodeChunk` owns one `Bytecode` plus scalar preparation context. Bytes, memory safety padding, jump analysis and the cached chunk hash belong to that Bytecode. There is no separate original-byte buffer or per-entry execution variant. `from_prepared(bytes, code_size, index)` restores persisted buffers and validates their local layout; providers authenticate them against the account's commitments at ingestion.

Preparation recognizes PUSH1 through PUSH32, the two-byte operands of RJUMP/RJUMPI and the one-byte operands of DUPN/SWAPN/EXCHANGE. Any immediate crossing a boundary is copied into the beginning of the preceding chunk's 32-byte padding field. Missing immediate bytes at the end of the contract are zero-filled. The original bytes at the start of the next chunk remain unchanged. Its final offset byte records how many of those bytes belong to the preceding instruction (`0..32`); jump analysis starts after them.

Unused padding is executable: with at least four bytes available it contains `RJUMP`, its two-byte operand, zero filler and a final `JUMPDEST`. The jump skips the filler and performs a normal destination check within the current physical buffer. With one to three bytes available it contains that many `JUMPDEST`s. Thus padding costs `min(unused bytes, 3)` gas, preserves the stack and adds no chunk reads. The interpreter's opcode loop needs no boundary checks.

The tail is `RJUMP -35` (`e0 5a 38`) when another logical instruction follows, otherwise STOP. The displacement accounts for the fixed 32-byte padding and the three-byte jump. Only this authenticated final RJUMP bypasses JUMPDEST validation: it loads the next chunk and uses that chunk's offset byte to select its first real opcode. A data-only final chunk need not be loaded by fallthrough. Inspectors omit generated padding and tails.

JUMP, taken JUMPI, ordinary RJUMP and taken RJUMPI require a JUMPDEST in the logical payload. Leading immediate bytes are never valid destinations, even when their value is `0x5b`. Ordinary jumps cannot address another chunk's padding through logical positions. There is no runtime bytecode rewriting based on entry offset.

`RJUMP` (`0xe0`) costs 2 gas and uses two EIP-8024-encoded immediate bytes. Each byte belongs to the DUPN/SWAPN alphabet `00..5a` or `80..ff`: decode with `decode_single`, subtract 17, and combine as `high * 219 + low`; values above 23980 subtract 47961. The signed range is -23980 through +23980, relative to the logical PC after the three-byte instruction. Ordinary targets must be inside logical code. Both relative opcodes are unknown unless TIP-1143 is enabled, including resident initcode.

`bytecode()` returns the shared Bytecode. `bytes()` returns its stored bytes without memory safety padding; `payload_len()` excludes the 32-byte padding, tail and offset byte.

CODECOPY and EXTCODECOPY copy original payload positions directly, excluding all appended fields. They do not fetch a predecessor to reconstruct leading data. All intersecting chunk tariffs are reserved before reading them. Failures propagate as usual; zero-length and out-of-range copies load no chunks.

## Account types and delegation

`AccountInfo::extension` is the only source of account code type. It is always available, including in builds without default features; there is no separate `account-ext` feature or opaque account payload.

- Empty extension: ordinary code, with emptiness determined by `code_hash`.
- Version 1: `AccountExtension::chunked(CodeMetadata)` contains logical code size and ordered prepared-buffer hashes. Metadata validates lengths 24,541 through 981,600 and exactly `ceil(size / 24540)` hashes.
- Version 2: `AccountExtension::delegated(Address)` contains the delegation target. The ordinary account code hash commits to its marker.

`AccountExtension::encode` emits empty bytes, `1 || code_size_be_u32 || chunk_hashes`, or `2 || target_20_bytes`. `decode` rejects unknown versions, invalid sizes/counts, malformed lengths and zero delegation targets. Serde uses this byte encoding and omits empty extensions from account records. Client account/trie codecs must preserve the selected type and commitments.

`AccountInfo::code_metadata()` and `delegation_target()` read the extension. `inline_delegation_code()` validates the target and marker hash, then synthesizes the marker without payload I/O or a chunk tariff. Providers must normalize historical delegation records into version 2 metadata when loading accounts. There is no execution-time kind lookup or fallback to a cached bytecode kind. Only one delegation hop is followed.

`CodeChunk::from_bytecode` preserves existing analysis and kind. `CodeChunk::new` treats bytes as legacy instructions, including delegation-shaped prefixes. `AccountHandle::set_chunked_code` explicitly selects version 1 when needed; `set_code_slow` does not infer chunking from size. Installing delegation code selects version 2. Replacing or clearing code replaces the extension.

Code setters, `set_extension`, and `set_info` invalidate account-local chunks immediately when code identity changes. No public account mutation method exposes an unrestricted mutable `AccountInfo`. Handle drop only journals its snapshot; rollback restores the account, cached chunks and warmth together.

## Provider and cache contract

Typed, erased, counted and asynchronous database interfaces require `get_code_chunk_by_hash(hash, index) -> Result<Option<CodeChunk>, Error>`. Code type comes from `get_account`; no separate kind lookup is required. There is no default multi-chunk full-code fallback. A chunked request returns the persisted prepared Bytecode and its scalar context. Providers authenticate its identity at ingestion; execution validates context without rehashing every payload.

Legacy account execution uses the explicit `get_code_by_hash` record operation after reserving its chunk-zero gas. This preserves an unchanged 24,576-byte legacy record even if another account with the same hash uses the new stride. The selected account representation, not descriptor presence or payload length, chooses the operation. Chunked requests must not use a cached full original record to hide missing prepared data.

`Cache::insert_code` records original code without automatically splitting historical records. Account insertion publishes prepared chunks only when account metadata selects that representation. Creation prepares its chunks before publication. Original and prepared caches retain immutable data across transaction reset and revert.

Prepared entries are keyed by `(global_code_hash, index)`. Each `CodeChunk` shares its Bytecode allocation and lazy analysis across clones; no separate analyzed-chunk map or alternate execution buffers are needed. Prepared hashes include the fixed padding, tails and offset bytes.

`State::load_code_chunk` returns a `CodeChunkHandle` after checking bounds, skipped cold accesses, payload length and preparation context. Loading does not change warmth or record a journal entry. The handle exposes `get()` and `is_warm()`; `warm()` marks access and journals the cold-to-warm transition, returning whether the chunk was cold. Rollback restores warmth while retaining cached bytes, as it does for storage reads. The host reserves all known chunk tariffs before loading, then explicitly warms the handle. Missing required data and corruption become node errors, never empty bytecode or EVM halts. Rejected entries are evicted through typed, cached, counted and async adapters. Physical residency never determines consensus warmth or price.

Account warmth is keyed by owner and index. Journals restore warmth and code identity at rollback; transaction clearing resets logical warmth while retaining immutable bytes.

## Execution and client handoff

`LoadedCode` and `MessageExt` carry one `code_chunk` container. `MessageExt::is_lazy_code` explicitly selects lazy runtime loading and chunk tariffs. It is false for resident initcode, synthesized delegation markers and pre-TIP frames. The interpreter obtains its active bytecode view from the chunk.

Both explicit jump opcodes use unchanged global positions. Untaken JUMPI does not read or warm its target. Cross-chunk jumps and internal fallthrough reserve destination gas before I/O. Copy operations read logical payload bytes directly from prepared buffers. Inline marker copies are synthesized from account metadata. Legacy EXTCODECOPY keeps its 24,576-byte source threshold, independent of the new runtime stride.

Draft frames, including resident initcode, run through the interpreter; JIT/AOT support is deferred. Inspectors must report original PCs and omit internal padding and tails. Creation limits apply through transaction, embedding, CREATE and CREATE2 paths, while ordinary gas, memory and deposit constraints remain.

Reth/Tempo must authenticate ingestion, atomically persist prepared buffers, scalar context and account type, preserve historical state views, reconstruct original RPC bytes, and use one coherent immutable dependency graph. Production inventory, replay, proof growth, provider latency, prepared-buffer costs and final tariff decisions remain activation prerequisites.


`RJUMPI` (`0xe1`) uses the same two-byte offset encoding and costs 4 gas. It pops one condition: nonzero jumps through the shared RJUMP target loader, while zero advances past the immediate. An untaken branch does not validate, load, charge or warm its target chunk; ordinary fallthrough can still cross a chunk boundary and execute a generated RJUMP. Invalid immediate encodings fail even for an untaken branch. Both relative opcodes are unknown when TIP-1143 is disabled.
