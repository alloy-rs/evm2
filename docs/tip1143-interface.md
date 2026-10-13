# TIP-1143 EVM2 draft

TIP-1143 is opt-in and unscheduled. A client enables it with
`Version::with_tip1143(true)`, sets `max_code_size` and `max_initcode_size`, and
enables `Version::with_rjump(true)` for the generated chunk transfers. RJUMP and
RJUMPI can be enabled independently of chunked code. The draft gas prices are
28,680 for a cold chunk and 1,000 for a warm chunk.

Code of at most 24 KiB remains ordinary legacy bytecode. Larger runtime code is
split into at most 40 payloads of 24 KiB minus 36 bytes. `AccountInfo::code_hash`
commits to the complete original code. The optional `AccountInfo::chunks` stores
the final payload length and ordered hashes of the prepared chunks. EXTCODEHASH
returns `code_hash`; EXTCODESIZE, CODECOPY and EXTCODECOPY use the original
logical bytes and exclude preparation bytes.

Each prepared chunk stores its original payload, exactly 32 bytes of padding, a
STOP or RJUMP transfer, and one trailing start offset. A PUSH or other immediate
that crosses a boundary keeps its original bytes in the next payload and copies
the missing suffix into the preceding chunk's padding. The offset records how
many leading bytes of the next payload are data, so its jump table begins at the
first actual opcode. The generated transfer alone can enter that opcode without
JUMPDEST; ordinary JUMP, JUMPI, RJUMP and RJUMPI require JUMPDEST, including
across chunks.

The state overlay caches requested chunks by account and index. Chunks are
obtained through `Database::get_code_by_hash` using the ordered chunk hashes.
The database must return `Bytecode::new_chunk` for a prepared hash and persist
those buffers with the account metadata. Chunk reads are metered and warmed
independently of account access. A missing or malformed committed chunk is a
fatal database error. Up to 2 MiB of initcode executes as one legacy bytecode
buffer; only successful creation output is split.

The account extension remains raw chain-specific data. A client using this draft
must persist the optional `chunks` field with the account and keep its code
records available by hash. Activation, provider integration, and gas calibration
remain client decisions.
