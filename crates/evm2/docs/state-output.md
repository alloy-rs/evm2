# State output and transaction lifecycle

This document describes the intended `evm2` state-output model. The goal is to
make the cheap paths cheap while still supporting owned materialized output when a caller needs it.

The core rule is: transaction execution should not imply state materialization.
Execution produces an outcome and leaves writes in a pending transaction layer.
The caller then explicitly commits, discards, or detaches that pending state.

## State layers

`evm2` state is split conceptually into three layers.

1. **Accepted overlay**

   The accepted overlay is transaction-boundary state. It contains all changes
   from transactions that the caller has committed so far and shadows the
   wrapped backing database. Later transactions read this overlay as part of
   their starting state.

2. **Transaction scratch**

   Transaction scratch contains writes, warm-access state, transient storage,
   the revert journal, touched accounts, selfdestruct markers, and logs for the
   currently executing transaction. This layer is reusable: after commit,
   discard, or detach it is cleared while retaining capacity where possible.

3. **Block accumulator**

   A block accumulator records block-level changes as transactions are
   committed. It keeps the first value observed at the block boundary and the
   latest value after the last committed transaction. At block end it can be
   frozen and exposed through borrowed or sorted views.

The accepted overlay is for execution correctness between transactions. The
block accumulator is for final block output. They are related, but they serve
different consumers.

## Lifecycle terms

### `transact`

`transact` validates and executes a transaction through the registered handler.
If execution reaches transaction finalization, the resulting post-finalization
writes remain in transaction scratch and the function returns a pending
transaction handle.

`transact` does **not** commit to the accepted overlay, does **not** update the
block accumulator, and does **not** build an owned `StateChanges` value unless a
materialization path asks for one later.

### Pending transaction

A pending transaction is an exclusive handle to the EVM while transaction scratch
contains the transaction's post-finalization writes. Because it borrows the EVM
mutably, another transaction cannot start until the pending handle is resolved.

A pending transaction must be resolved in one of these ways:

- `commit`
- `commit_to`
- `commit_with`
- `discard`
- `detach`

If a pending handle is dropped without being resolved, it is treated as
`discard` so scratch cannot leak into the next transaction.

### `commit`

`commit` accepts the pending transaction.

It means:

- apply pending writes to the accepted overlay so later transactions can read
  them;
- clear transaction scratch for reuse;
- return the transaction outcome.

`commit` is the normal serial block-execution path. A reverted EVM transaction
can still be committed: the transaction outcome may have `status = false`, but
transaction-level effects such as nonce/gas accounting are still accepted if the
handler finalized successfully.

### `discard`

`discard` rejects the pending transaction's state.

It means:

- do not mutate the accepted overlay;
- do not mutate the block accumulator;
- do not stream writes to state sinks;
- clear transaction scratch for reuse;
- return the transaction outcome.

Use `discard` for result-only execution such as `eth_call`, gas estimation trial
runs, or conditional execution where the caller decided not to accept the
transaction.

Important: after `discard`, the transaction's writes are **not** visible to the
next transaction.

### `detach`

`detach` materializes an owned transaction diff and clears scratch.

It means:

- build an owned `StateChanges`/owned transaction result from transaction
  scratch;
- do not mutate the accepted overlay;
- do not mutate the block accumulator;
- clear transaction scratch for reuse;
- return owned state that can be moved, stored, sent to another worker, or
  committed later by the caller.

Use `detach` for debugging/tracing that needs an owned write-set, and
parallel/BAL-style workers that need to move a transaction diff between threads.

The difference between `discard` and `detach` is ownership of the writes:

- `discard` drops the writes;
- `detach` keeps the writes in an owned materialized value.

Neither one commits the writes to the accepted overlay.

### `commit_to`

`commit_to` accepts the pending transaction and records the same borrowed change
stream in a block accumulator.

It means:

- stream pending writes into the supplied `BlockStateAccumulator`;
- apply pending writes to the accepted overlay so later transactions can read
  them;
- clear transaction scratch for reuse;
- return the transaction outcome.

Use `commit_to` for serial block execution when the caller wants block-level
coalesced state output.

### `commit_with`

`commit_with` streams pending writes into an arbitrary sink before accepting the
transaction. Use it to feed tracing, witness, trie, cache, or tee sinks without
building an owned `StateChanges`.

### `freeze`

`freeze` finishes a block accumulator and returns immutable block state. Frozen
state exposes borrowed account/storage/code iteration and sorted account/storage views. Sorting
happens at freeze/view time, not per transaction.

## Execution outcomes and logs

Logs are execution output. They are not database state.

`TxOutcome` carries logs together with status, gas, output, stop reason, database
error handle, and extension data. Detached `TxResult` values also carry logs next to the
materialized `StateChanges`; the state diff itself contains database changes only. Normal block
execution can build receipts from `TxOutcome` without materializing a state diff.

## Error and status behavior

- **Successful execution** returns a pending transaction whose outcome has
  `status = true`. The caller chooses `commit`, `commit_to`, `commit_with`,
  `discard`, or `detach`.
- **EVM revert/halt** can still return a pending transaction. The outcome records
  the failed status/stop/output, while transaction-level state effects remain
  pending if transaction finalization completed.
- **Invalid transaction / handler error** returns a handler error and clears
  transaction scratch. There is no pending transaction to resolve.
- **Database error during execution/finalization** is recorded in the outcome's
  database error handle when execution can produce a transaction result. If no
  valid pending state remains, resolving the pending handle is a no-op for state.

## Allocation guide

Cheapest paths:

```text
eth_call / simulation: transact -> discard
serial block:          transact -> commit
```

Materializing paths:

```text
materialized tx diff: transact -> detach -> TxResult
parallel worker:      transact -> detach -> send owned diff
block accumulator:    transact -> commit_to -> FrozenBlockState
```

The design aims for the serial block path to walk transaction scratch once and
fan out changes to the accepted overlay, block accumulator, and optional sinks.
Owned `StateChanges` is a materialized view, not a mandatory hot-path intermediate.

## Source and sink terminology

A **source** exposes state changes. Examples: transaction scratch, owned
`StateChanges`, frozen block state.

A **sink** consumes changes. Examples: block accumulator, hashed-trie updater,
execution cache, witness recorder, or test recorder.

The source/sink API is a borrowed visitor over state changes. It lets the common
path stream changes directly from scratch into multiple consumers without first
building an owned per-transaction map.

## Example flows

### Result-only call

```rust,ignore
let pending = evm.transact(&tx)?;
let outcome = pending.discard();
assert!(!outcome.logs.is_empty() || outcome.output.is_empty());
```

### Serial block execution

```rust,ignore
let mut block_state = BlockStateAccumulator::new();

for tx in block.transactions() {
    let pending = evm.transact(tx)?;
    receipt_builder.observe(pending.outcome());
    let outcome = pending.commit_to(&mut block_state);
    receipts.push(receipt_builder.finish(outcome));
}

let frozen_state = block_state.freeze();
```

### Conditional commit

```rust,ignore
let pending = evm.transact(&tx)?;
if should_accept(pending.outcome()) {
    pending.commit_to(&mut block_state);
} else {
    pending.discard();
}
```

### Detached worker

```rust,ignore
let pending = worker_evm.transact(&tx)?;
let owned = pending.detach();
work_queue.send(owned)?;
```

### Materialized output

```rust,ignore
let materialized: TxResult<_> = evm.transact(&tx)?.detach();
```
