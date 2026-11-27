//! Match dispatch: the whole frame runs in one function.
//!
//! The loop matches on the opcode and every arm holds the handler inlined with a constant opcode,
//! so the program counter, stack length, gas and stack base stay in registers across
//! instructions, with no call between them. Each arm ends in its own exit check and jumps back to
//! the match, which lets the compiler copy the match's indirect jump into the arms so that each
//! one gets its own branch prediction history.
//!
//! Only frames that run without an inspector use it, on the config its function was
//! instantiated for. The others keep the table loop.

use crate::interpreter::{InstrStop, Interpreter};

/// Runs a frame without an inspector to its stop.
///
/// Calling it requires a prepared interpreter, as for the loops themselves.
pub(crate) type MatchLoop<T> = unsafe fn(&mut Interpreter<'_, '_, T>) -> InstrStop;

/// The loops, defined in a few modules of their own and named by spec.
///
/// The compiler puts all instances of a function in one codegen unit, so the loops would be
/// optimized in one thread, one after the other. Separate definitions of the same source spread
/// them over several codegen units.
pub(in crate::interpreter) mod loops {
    macro_rules! loop_modules {
        ($($group:ident),*) => {
            $(
                pub(in crate::interpreter) mod $group {
                    include!("match_loop_body.rs");
                }
            )*
        };
    }

    loop_modules!(g0, g1, g2, g3);

    pub(in crate::interpreter) use g0 as frontier;
    pub(in crate::interpreter) use g0 as homestead;
    pub(in crate::interpreter) use g0 as tangerine;
    pub(in crate::interpreter) use g0 as spurious_dragon;
    pub(in crate::interpreter) use g1 as byzantium;
    pub(in crate::interpreter) use g1 as petersburg;
    pub(in crate::interpreter) use g1 as istanbul;
    pub(in crate::interpreter) use g1 as berlin;
    pub(in crate::interpreter) use g2 as london;
    pub(in crate::interpreter) use g2 as merge;
    pub(in crate::interpreter) use g2 as shanghai;
    pub(in crate::interpreter) use g2 as cancun;
    pub(in crate::interpreter) use g3 as prague;
    pub(in crate::interpreter) use g3 as osaka;
    pub(in crate::interpreter) use g3 as amsterdam;
    pub(in crate::interpreter) use g3 as custom;
}
