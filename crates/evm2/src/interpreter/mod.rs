pub(crate) type Result<T = (), E = InstrErr> = core::result::Result<T, E>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SpecId {
    Frontier,
    Homestead,
}

#[derive(Clone, Copy, Debug)]
pub enum InstrErr {
    Stop = 1,
    OutOfGas,
    StackOverflow,
    StackUnderflow,
    Invalid,
}

mod gas;
pub use gas::{Gas, GasRef};

#[macro_use]
mod utils;

mod instruction;
pub use instruction::*;

mod instructions;

mod opcode;
pub use opcode::op;

mod pc;
pub use pc::{Pc, PcRef};

mod stack;
pub use stack::{Stack, Word};

mod state;
pub use state::{Host, State};

mod runtime;
pub use runtime::{Interpreter, Table};

mod table;
pub use table::{
    DEFAULT_GAS_TABLE, DEFAULT_TABLE, DEFAULT_TAIL_TABLE, make_table, make_tail_table, mk_dispatch,
    mk_tail_dispatch, new_gas_table,
};

#[cfg(test)]
mod tests {
    use alloy_primitives::U256;

    use super::*;

    struct DummyHost;

    impl Host for DummyHost {
        fn balance(&self, address: Word) -> Word {
            address
        }
    }

    #[test]
    fn main_smoke() {
        #[rustfmt::skip]
        let bytecode = core::hint::black_box(&[
            op::PUSH1, 0x01,
            op::PUSH1, 0x02,
            op::ADD,
            op::STOP,
        ][..]);
        let spec_id = core::hint::black_box(SpecId::Homestead);
        let instruction_table = core::hint::black_box(Table::Tail(&DEFAULT_TAIL_TABLE));

        let gas_table = new_gas_table(spec_id);
        let mut interpreter = Interpreter::new(bytecode.into(), spec_id);
        interpreter.run(instruction_table, &gas_table, &mut DummyHost);
    }

    #[test]
    fn basic() {
        const BASIC: &[u8] = &[op::PUSH1, 0x01, op::PUSH1, 0x02, op::ADD, op::STOP];

        for spec in [SpecId::Frontier, SpecId::Homestead] {
            let gas_table = new_gas_table(spec);
            for (_name, table) in [
                ("normal", Table::Normal(&DEFAULT_TABLE)),
                ("tail", Table::Tail(&DEFAULT_TAIL_TABLE)),
            ] {
                let mut interpreter = Interpreter::new(BASIC.into(), spec);
                interpreter.run(table, &gas_table, &mut DummyHost);
                assert!(interpreter.gas.remaining > 0);
                assert_eq!(interpreter.pc, 6);
                assert_eq!(interpreter.stack_len, 1);
                assert_eq!(interpreter.stack[0], U256::from(3));
            }
        }
    }
}
