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

pub(super) trait ToWord {
    fn to_word(self) -> Word;
}

impl ToWord for Word {
    fn to_word(self) -> Word {
        self
    }
}

impl ToWord for i32 {
    fn to_word(self) -> Word {
        Word::from(self as u64)
    }
}

impl ToWord for u64 {
    fn to_word(self) -> Word {
        Word::from(self)
    }
}

impl ToWord for usize {
    fn to_word(self) -> Word {
        Word::from(self)
    }
}

pub(super) fn run_stack<T: ToWord, const N: usize>(inputs: [T; N], opcode: u8) -> TestInterpreter {
    let mut code = Vec::new();
    for input in inputs {
        push(&mut code, input);
    }
    code.extend([opcode, op::STOP]);
    run(code)
}

pub(super) fn assert_stack_words(inputs: &[Word], opcode: u8, expected: &[Word]) {
    let mut code = Vec::new();
    for input in inputs {
        push(&mut code, *input);
    }
    code.extend([opcode, op::STOP]);
    let interpreter = run(code);
    assert!(matches!(interpreter.err, InstrErr::Stop));
    assert_eq!(interpreter.stack(), expected);
}

macro_rules! assert_stack {
    ($op:ident($($input:expr),* $(,)?), $expected:expr $(,)?) => {{
        let inputs = [$($crate::interpreter::Word::from($input)),*];
        let expected = [$crate::interpreter::Word::from($expected)];
        $crate::interpreter::instructions::tests::assert_stack_words(
            &inputs,
            $crate::interpreter::op::$op,
            &expected,
        );
    }};
}
pub(super) use assert_stack;

pub(super) fn push(code: &mut Vec<u8>, value: impl ToWord) {
    let value = value.to_word();
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
