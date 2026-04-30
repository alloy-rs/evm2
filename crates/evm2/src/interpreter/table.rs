use super::{
    SpecId,
    instruction::{GasTable, InstrTable, TailInstrTable},
    instructions::{add_impl, balance_impl, invalid_impl, push_impl, stop_impl},
    opcode::{for_each_opcode, op},
};

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
        ($op:ident, $fn:expr),
    )*) => {
        $(
            $table[op::$op as usize] = $fn;
        )*
    };
}
macro_rules! make_table_m {
    () => {{
        let mut table: InstrTable = [invalid_impl; 256];
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
