//! EVM constants.

/// Maximum deployed contract bytecode size.
///
/// EIP-170 - Contract code size limit.
pub(crate) const MAX_CODE_SIZE: usize = 0x6000;

/// Maximum contract creation initcode size.
///
/// EIP-3860 - Limit and meter initcode.
pub(crate) const MAX_INITCODE_SIZE: usize = 2 * MAX_CODE_SIZE;

/// Maximum message call depth.
pub(crate) const CALL_DEPTH_LIMIT: u16 = 1024;

/// Maximum EVM stack height.
pub(crate) const STACK_LIMIT: usize = 1024;

/// Number of recent block hashes available to the `BLOCKHASH` opcode.
pub(crate) const BLOCK_HASH_HISTORY: u64 = 256;

/// EIP-7702 version magic.
pub(crate) const EIP7702_MAGIC: u16 = 0xEF01;
/// EIP-7702 version magic bytes.
pub(crate) const EIP7702_MAGIC_BYTES: &[u8] = &EIP7702_MAGIC.to_be_bytes();
/// EIP-7702 version.
pub(crate) const EIP7702_VERSION: u8 = 0;
/// EIP-7702 bytecode length.
///
/// 2 (magic) + 1 (version) + 20 (address) = 23 bytes.
pub(crate) const EIP7702_BYTECODE_LEN: usize = 23;
