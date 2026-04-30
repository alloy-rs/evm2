use crate::interpreter::{
    DEFAULT_GAS_TABLE, DEFAULT_TABLE, Host, InstrErr, Interpreter, SpecId, Table, Word, op,
};
use alloc::vec::Vec;

pub(super) struct TestHost;

impl Host for TestHost {
    fn balance(&self, address: Word) -> Word {
        address
    }
}

pub(super) struct TestInterpreter {
    pub(super) inner: Interpreter,
    pub(super) err: InstrErr,
}

impl TestInterpreter {
    pub(super) fn stack(&self) -> &[Word] {
        unsafe { core::slice::from_raw_parts(self.inner.stack.as_ptr(), self.inner.stack_len) }
    }

    pub(super) fn memory(&mut self, offset: usize, len: usize) -> &[u8] {
        self.inner.memory.slice(offset, len).unwrap()
    }
}

pub(super) fn run(code: impl Into<Vec<u8>>) -> TestInterpreter {
    let mut inner = Interpreter::new(code.into(), SpecId::Homestead);
    let err = inner.run(Table::Normal(&DEFAULT_TABLE), &DEFAULT_GAS_TABLE, &mut TestHost);
    TestInterpreter { inner, err }
}

pub(super) fn run_stack(inputs: &[Word], opcode: u8) -> TestInterpreter {
    let mut code = Vec::new();
    for input in inputs {
        push(&mut code, *input);
    }
    code.extend([opcode, op::STOP]);
    run(code)
}

pub(super) fn push(code: &mut Vec<u8>, value: Word) {
    if value.is_zero() {
        code.push(op::PUSH0);
        return;
    }

    let bytes = value.to_be_bytes::<32>();
    let start = bytes.iter().position(|&byte| byte != 0).unwrap();
    let len = bytes.len() - start;
    code.push(op::PUSH1 + len as u8 - 1);
    code.extend_from_slice(&bytes[start..]);
}
