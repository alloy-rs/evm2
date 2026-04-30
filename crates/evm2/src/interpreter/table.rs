use super::{
    CtrlRef, GasRef, InstrErr, InstructionCx, Result, SpecId, Stack, State, Word,
    instruction::{GasTable, InstrFnRet, InstrTable, TailInstrTable},
    instructions::{add_impl, balance_impl, invalid_impl, push_impl, stop_impl},
    opcode::{for_each_opcode, op},
};
use core::hint::cold_path;

pub static DEFAULT_TABLE: InstrTable = make_table();
pub static DEFAULT_TAIL_TABLE: TailInstrTable = make_tail_table();

pub static DEFAULT_GAS_TABLE: GasTable = [3; 256];

pub fn new_gas_table(spec: SpecId) -> GasTable {
    let mut t = DEFAULT_GAS_TABLE;
    if spec >= SpecId::Homestead {
        t[op::ADD as usize] = 69;
    }
    t
}

macro_rules! make_table_inner {
    ([$table:expr] $(
        ($op:ident, $_fn:expr),
    )*) => {
        $(
            $table[op::$op as usize] = $op;
        )*
    };
}
macro_rules! table_wrapper {
    (PUSH0, $fn:expr) => {
        push_wrapper!(PUSH0, $fn);
    };
    (PUSH1, $fn:expr) => {
        push_wrapper!(PUSH1, $fn);
    };
    (PUSH2, $fn:expr) => {
        push_wrapper!(PUSH2, $fn);
    };
    (PUSH3, $fn:expr) => {
        push_wrapper!(PUSH3, $fn);
    };
    (PUSH4, $fn:expr) => {
        push_wrapper!(PUSH4, $fn);
    };
    (PUSH5, $fn:expr) => {
        push_wrapper!(PUSH5, $fn);
    };
    (PUSH6, $fn:expr) => {
        push_wrapper!(PUSH6, $fn);
    };
    (PUSH7, $fn:expr) => {
        push_wrapper!(PUSH7, $fn);
    };
    (PUSH8, $fn:expr) => {
        push_wrapper!(PUSH8, $fn);
    };
    (PUSH9, $fn:expr) => {
        push_wrapper!(PUSH9, $fn);
    };
    (PUSH10, $fn:expr) => {
        push_wrapper!(PUSH10, $fn);
    };
    (PUSH11, $fn:expr) => {
        push_wrapper!(PUSH11, $fn);
    };
    (PUSH12, $fn:expr) => {
        push_wrapper!(PUSH12, $fn);
    };
    (PUSH13, $fn:expr) => {
        push_wrapper!(PUSH13, $fn);
    };
    (PUSH14, $fn:expr) => {
        push_wrapper!(PUSH14, $fn);
    };
    (PUSH15, $fn:expr) => {
        push_wrapper!(PUSH15, $fn);
    };
    (PUSH16, $fn:expr) => {
        push_wrapper!(PUSH16, $fn);
    };
    (PUSH17, $fn:expr) => {
        push_wrapper!(PUSH17, $fn);
    };
    (PUSH18, $fn:expr) => {
        push_wrapper!(PUSH18, $fn);
    };
    (PUSH19, $fn:expr) => {
        push_wrapper!(PUSH19, $fn);
    };
    (PUSH20, $fn:expr) => {
        push_wrapper!(PUSH20, $fn);
    };
    (PUSH21, $fn:expr) => {
        push_wrapper!(PUSH21, $fn);
    };
    (PUSH22, $fn:expr) => {
        push_wrapper!(PUSH22, $fn);
    };
    (PUSH23, $fn:expr) => {
        push_wrapper!(PUSH23, $fn);
    };
    (PUSH24, $fn:expr) => {
        push_wrapper!(PUSH24, $fn);
    };
    (PUSH25, $fn:expr) => {
        push_wrapper!(PUSH25, $fn);
    };
    (PUSH26, $fn:expr) => {
        push_wrapper!(PUSH26, $fn);
    };
    (PUSH27, $fn:expr) => {
        push_wrapper!(PUSH27, $fn);
    };
    (PUSH28, $fn:expr) => {
        push_wrapper!(PUSH28, $fn);
    };
    (PUSH29, $fn:expr) => {
        push_wrapper!(PUSH29, $fn);
    };
    (PUSH30, $fn:expr) => {
        push_wrapper!(PUSH30, $fn);
    };
    (PUSH31, $fn:expr) => {
        push_wrapper!(PUSH31, $fn);
    };
    (PUSH32, $fn:expr) => {
        push_wrapper!(PUSH32, $fn);
    };
    (ADD, $fn:expr) => {
        extern_table! {
            #[inline]
            #[allow(non_snake_case)]
            fn ADD(
                _ctrl: CtrlRef<'_>,
                mut stack: Stack<'_>,
                _gas: GasRef<'_>,
                _state: &mut State<'_>,
            ) -> InstrFnRet {
                let r = (|| -> Result {
                    if stack.len < 2 {
                        cold_path();
                        return Err(InstrErr::StackUnderflow);
                    }
                    let ptr = unsafe { stack.stack.as_mut_ptr().add(stack.len).sub(2) };
                    let [a, b] = unsafe { &*ptr.cast::<[Word; 2]>() };
                    let out = unsafe { &mut *ptr.cast::<Word>() };
                    stack.len -= 1;
                    $fn(a, b, out)
                })();
                (stack.len, r)
            }
        }
    };
    (BALANCE, $fn:expr) => {
        extern_table! {
            #[inline]
            #[allow(non_snake_case)]
            fn BALANCE(
                mut ctrl: CtrlRef<'_>,
                stack: Stack<'_>,
                gas: GasRef<'_>,
                state: &mut State<'_>,
            ) -> InstrFnRet {
                let r = (|| -> Result {
                    if stack.len < 1 {
                        cold_path();
                        return Err(InstrErr::StackUnderflow);
                    }
                    let ptr = unsafe { stack.stack.as_mut_ptr().add(stack.len).sub(1) };
                    let [addr] = unsafe { &*ptr.cast::<[Word; 1]>() };
                    let out = unsafe { &mut *ptr.cast::<Word>() };
                    let mut cx = InstructionCx { ctrl: &mut ctrl, gas, host: &mut *state.host };
                    $fn(&mut cx, addr, out)
                })();
                (stack.len, r)
            }
        }
    };
    ($op:ident, $fn:expr) => {
        extern_table! {
            #[inline]
            #[allow(non_snake_case)]
            fn $op(
                _ctrl: CtrlRef<'_>,
                stack: Stack<'_>,
                _gas: GasRef<'_>,
                _state: &mut State<'_>,
            ) -> InstrFnRet {
                let r = $fn();
                (stack.len, r)
            }
        }
    };
}
macro_rules! push_wrapper {
    ($op:ident, $fn:expr) => {
        extern_table! {
            #[inline]
            #[allow(non_snake_case)]
            fn $op(
                mut ctrl: CtrlRef<'_>,
                mut stack: Stack<'_>,
                gas: GasRef<'_>,
                state: &mut State<'_>,
            ) -> InstrFnRet {
                let r = (|| -> Result {
                    if stack.len == 1024 {
                        cold_path();
                        return Err(InstrErr::StackOverflow);
                    }
                    let ptr = unsafe { stack.stack.as_mut_ptr().add(stack.len) };
                    let out = unsafe { &mut *ptr.cast::<Word>() };
                    stack.len += 1;
                    let mut cx = InstructionCx { ctrl: &mut ctrl, gas, host: &mut *state.host };
                    $fn(&mut cx, out)
                })();
                (stack.len, r)
            }
        }
    };
}
macro_rules! table_wrappers {
    ([$($extra:tt)*] $(
        ($op:ident, $fn:expr),
    )*) => {
        $(
            table_wrapper!($op, $fn);
        )*
    };
}
for_each_opcode!([] table_wrappers);

macro_rules! make_table_m {
    () => {{
        let mut table: InstrTable = [INVALID; 256];
        for_each_opcode!([table] make_table_inner);
        table
    }};
}

pub const fn make_table() -> InstrTable {
    make_table_m!()
}

pub const fn make_tail_table() -> TailInstrTable {
    make_table()
}
