use evm2_jit_backend::{
    Attribute, Backend, Builder, CallConv, FunctionAttributeLocation, TypeMethods,
};

// Must be kept in sync with `evm2-jit-build`.
const MANGLE_PREFIX: &str = "__revmc_builtin_";

/// Builtin cache.
#[derive(Debug)]
pub struct Builtins<B: Backend>(
    [Option<B::Function>; Builtin::COUNT],
    Option<fn(Builtin) -> usize>,
);

unsafe impl<B: Backend> Send for Builtins<B> {}

impl<B: Backend> Default for Builtins<B> {
    fn default() -> Self {
        Self::new()
    }
}

impl<B: Backend> Builtins<B> {
    /// Create a new cache.
    pub const fn new() -> Self {
        Self([None; Builtin::COUNT], None)
    }

    /// Selects host-specific builtin addresses.
    pub fn set_resolver(&mut self, resolver: fn(Builtin) -> usize) {
        self.clear();
        self.1 = Some(resolver);
    }

    /// Clear the cache.
    pub const fn clear(&mut self) {
        self.0 = [None; Builtin::COUNT];
    }

    /// Get the function for the given builtin.
    pub fn get(&mut self, builtin: Builtin, bcx: &mut B::Builder<'_>) -> B::Function {
        let resolver = self.1;
        *self.0[builtin as usize].get_or_insert_with(|| Self::init(builtin, bcx, resolver))
    }

    #[cold]
    fn init(
        builtin: Builtin,
        bcx: &mut B::Builder<'_>,
        resolver: Option<fn(Builtin) -> usize>,
    ) -> B::Function {
        let address = resolver.map_or_else(|| builtin.addr(), |resolve| resolve(builtin));
        let resolved_name = resolver.map(|_| format!("{}_{address:x}", builtin.name()));
        let name = resolved_name.as_deref().unwrap_or_else(|| builtin.name());
        debug_assert!(name.starts_with(MANGLE_PREFIX), "{name:?}");
        if builtin.call_conv() == CallConv::Default
            && let Some(r) = bcx.get_function(name)
        {
            trace!(name, ?r, "pre-existing");
            return r;
        }

        let r = Self::build(name, builtin, address, bcx);
        trace!(name, ?r, "built");
        r
    }

    fn build(
        name: &str,
        builtin: Builtin,
        address: usize,
        bcx: &mut B::Builder<'_>,
    ) -> B::Function {
        let ret = builtin.ret(bcx);
        let params = builtin.params(bcx);
        let linkage = evm2_jit_backend::Linkage::Import;
        let f = bcx.add_function(name, &params, ret, Some(address), linkage, CallConv::Default);
        Self::add_attrs(builtin, f, bcx);
        match builtin.call_conv() {
            CallConv::Default => f,
            call_conv => {
                let f = bcx.add_function_stub(f, call_conv);
                Self::add_attrs(builtin, f, bcx);
                f
            }
        }
    }

    fn add_attrs(builtin: Builtin, f: B::Function, bcx: &mut B::Builder<'_>) {
        let param_attrs = builtin.param_attrs();
        let mut attrs = Vec::with_capacity(16);
        attrs.extend(builtin.attrs());
        attrs.extend([Attribute::NoFree, Attribute::NoSync, Attribute::NoUnwind]);
        // `argmem` is only valid if the function does not access memory reachable through pointers
        // *loaded* from its arguments (only memory directly derived from arguments via
        // GEP/bitcast). Builtins that take a writable `EvmContext` mutate state through
        // `ecx.host`, `ecx.gas`, etc., which are loaded from `ecx` and thus outside
        // `argmem`. Apply `ArgMemOnly` only when no parameter is a writable `EvmContext`.
        let evm_ctx_size =
            core::mem::size_of::<evm2_jit_context::EvmContext<'static, 'static, 'static>>() as u64;
        let writes_ecx = param_attrs.iter().any(|p| {
            let writable = p.iter().any(|a| matches!(a, Attribute::Writable));
            let is_ecx =
                p.iter().any(|a| matches!(a, Attribute::Dereferenceable(s) if *s == evm_ctx_size));
            writable && is_ecx
        });
        if !writes_ecx {
            attrs.push(Attribute::ArgMemOnly);
        }
        for attr in attrs {
            bcx.add_function_attribute(Some(f), attr, FunctionAttributeLocation::Function);
        }
        for (i, param_attrs) in param_attrs.iter().enumerate() {
            for attr in param_attrs {
                bcx.add_function_attribute(
                    Some(f),
                    *attr,
                    FunctionAttributeLocation::Param(i as u32),
                );
            }
        }
    }
}

macro_rules! builtins {
    (@count) => { 0 };
    (@count $first:tt $(, $rest:tt)*) => { 1 + builtins!(@count $($rest),*) };

    (@param_attr $default:ident) => { $default() };
    (@param_attr $default:ident $name:expr) => { $name };

    (@types |$bcx:ident| { $($types_init:tt)* }
     @param_attrs |$op:ident| { $($attrs_init:tt)* }
     $($ident:ident = $(#[$attr:expr])* $name:ident($($(@[$param_attr:expr])? $params:expr),* $(,)?) $ret:expr),* $(,)?
    ) => { paste::paste! {
        /// Builtins that can be called by the compiled functions.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Builtin {
            $($ident,)*
        }

        #[allow(unused_variables)]
        impl Builtin {
            pub const COUNT: usize = builtins!(@count $($ident),*);
            pub const ALL: &[Self; Self::COUNT] = &[$(Self::$ident),*];

            pub const fn name(self) -> &'static str {
                match self {
                    $(Self::$ident => stringify!($name),)*
                }
            }

            pub fn addr(self) -> usize {
                match self {
                    $(Self::$ident => crate::$name as *const () as usize,)*
                }
            }

            pub fn parse(name: &str) -> Option<Self> {
                match name {
                    $(stringify!($name) => Some(Self::$ident),)*
                    _ => None,
                }
            }

            pub fn ret<B: TypeMethods>(self, $bcx: &mut B) -> Option<B::Type> {
                $($types_init)*
                match self {
                    $(Self::$ident => $ret,)*
                }
            }

            pub fn params<B: TypeMethods>(self, $bcx: &mut B) -> Vec<B::Type> {
                $($types_init)*
                match self {
                    $(Self::$ident => vec![$($params),*],)*
                }
            }

            pub const fn attrs(self) -> &'static [Attribute] {
                #[allow(unused_imports)]
                use Attribute::*;
                match self {
                    $(Self::$ident => &[$($attr),*]),*
                }
            }

            #[allow(non_upper_case_globals)]
            pub fn param_attrs(self) -> Vec<Vec<Attribute>> {
                #[allow(unused_imports)]
                use Attribute::*;
                let $op = self;
                let default = || vec![Attribute::NoUndef];
                $($attrs_init)*
                match self {
                    $(Self::$ident => vec![$(builtins!(@param_attr default $($param_attr)?)),*]),*
                }
            }

            const fn op(self) -> u8 {
                use evm2::interpreter::op::*;

                // _in_out
                const _0_0: u8 = STOP;
                const _0_1: u8 = PUSH0;

                const PANIC: u8 = _0_0;
                const ASSERTSPECID: u8 = _0_0;

                const KECCAK256CC: u8 = _0_1;

                const CALLDATALOADC: u8 = _0_1;
                const SLOADC: u8 = _0_1;

                const MRESIZE: u8 = _0_0;

                const LOG: u8 = _0_0;
                const DORETURN: u8 = RETURN;
                const DORETURNCC: u8 = _0_0;

                match self {
                    $(Self::$ident => [<$ident:upper>]),*
                }
            }
        }
    }};
}

builtins! {
    @types |bcx| {
        let ptr = bcx.type_ptr();
        let usize = bcx.type_ptr_sized_int();
        let u8 = bcx.type_int(8);
        let u32 = bcx.type_int(32);
    }

    @param_attrs |op| {
        fn size_and_align<T>() -> Vec<Attribute> {
            size_and_align_with(Some(core::mem::size_of::<T>()), core::mem::align_of::<T>())
        }

        fn size_and_align_with(size: Option<usize>, align: usize) -> Vec<Attribute> {
            let mut vec = Vec::with_capacity(8);
            vec.push(Attribute::NoAlias);
            vec.push(Attribute::NoCapture);
            vec.push(Attribute::NoUndef);
            vec.push(Attribute::Align(align as u64));
            if let Some(size) = size {
                vec.push(Attribute::Dereferenceable(size as u64));
            }
            vec
        }

        let op = op.op();
        let (inputs, outputs) = if let Some(info) = evm2::interpreter::opcode::OPCODE_INFO[op as usize] {
            (info.inputs(), info.outputs())
        } else {
            (0, 0)
        };

        let ecx_base = size_and_align::<evm2_jit_context::EvmContext<'static, 'static, 'static>>();
        let mut ecx = ecx_base.clone();
        ecx.push(Attribute::Writable);
        let mut ecx_ro = ecx_base;
        ecx_ro.push(Attribute::ReadOnly);

        let sp_base = size_and_align_with(None, core::mem::align_of::<evm2_jit_context::EvmWord>());
        let mut sp_dyn = sp_base.clone();
        sp_dyn.push(Attribute::Writable);

        let mut sp = sp_base;
        // `sp` is at `top - inputs`, we have access to `max(inputs, outputs)` words.
        let n_stack_words = inputs.max(outputs);
        let size_of_word = core::mem::size_of::<evm2_jit_context::EvmWord>();
        sp.push(Attribute::Dereferenceable(size_of_word as u64 * n_stack_words as u64));
        match (inputs, outputs) {
            (0, 0) => sp.push(Attribute::ReadNone),
            (0, 1..) => {
                sp.push(Attribute::Writable);
                sp.push(Attribute::WriteOnly);
                sp.push(Attribute::Initializes(size_of_word as u64 * outputs as u64));
            }
            (1.., 0) => sp.push(Attribute::ReadOnly),
            (1.., 1..) => sp.push(Attribute::Writable),
        }
    }

    Panic          = #[Cold] #[NoReturn] __revmc_builtin_panic(ptr, usize) None,
    AssertSpecId   = __revmc_builtin_assert_spec_id(@[ecx] ptr, u32) None,

    Div            = __revmc_builtin_div(@[sp] ptr) None,
    SDiv           = __revmc_builtin_sdiv(@[sp] ptr) None,
    Mod            = __revmc_builtin_mod(@[sp] ptr) None,
    SMod           = __revmc_builtin_smod(@[sp] ptr) None,
    AddMod         = __revmc_builtin_addmod(@[sp] ptr) None,
    MulMod         = __revmc_builtin_mulmod(@[sp] ptr) None,
    Exp            = __revmc_builtin_exp(@[ecx] ptr, @[sp] ptr) None,
    Keccak256      = __revmc_builtin_keccak256(@[ecx] ptr, @[sp] ptr) None,
    Keccak256CC    = __revmc_builtin_keccak256_cc(@[ecx] ptr, @[sp] ptr, usize, usize) None,
    Balance        = __revmc_builtin_balance(@[ecx] ptr, @[sp] ptr) None,
    CallDataLoad   = __revmc_builtin_calldataload(@[ecx_ro] ptr, @[sp] ptr) None,
    CallDataLoadC  = __revmc_builtin_calldataload_c(@[ecx_ro] ptr, @[sp] ptr, usize) None,
    CallDataCopy   = __revmc_builtin_calldatacopy(@[ecx] ptr, @[sp] ptr) None,
    CodeCopy       = __revmc_builtin_codecopy(@[ecx] ptr, @[sp] ptr) None,
    ExtCodeSize    = __revmc_builtin_extcodesize(@[ecx] ptr, @[sp] ptr) None,
    ExtCodeCopy    = __revmc_builtin_extcodecopy(@[ecx] ptr, @[sp] ptr) None,
    ReturnDataCopy = __revmc_builtin_returndatacopy(@[ecx] ptr, @[sp] ptr) None,
    ExtCodeHash    = __revmc_builtin_extcodehash(@[ecx] ptr, @[sp] ptr) None,
    BlockHash      = __revmc_builtin_blockhash(@[ecx] ptr, @[sp] ptr) None,
    SelfBalance    = __revmc_builtin_self_balance(@[ecx] ptr, @[sp] ptr) None,
    Difficulty     = __revmc_builtin_difficulty(@[ecx_ro] ptr, @[sp] ptr) None,
    BlobHash       = __revmc_builtin_blob_hash(@[ecx_ro] ptr, @[sp] ptr) None,
    Mresize        = __revmc_builtin_mresize(@[ecx] ptr, usize) None,
    Sload          = __revmc_builtin_sload(@[ecx] ptr, @[sp] ptr) None,
    SloadC         = __revmc_builtin_sload_c(@[ecx] ptr, @[sp] ptr, usize) None,
    Sstore         = __revmc_builtin_sstore(@[ecx] ptr, @[sp] ptr) None,
    Tload          = __revmc_builtin_tload(@[ecx] ptr, @[sp] ptr) None,
    Tstore         = __revmc_builtin_tstore(@[ecx] ptr, @[sp] ptr) None,
    Mcopy          = __revmc_builtin_mcopy(@[ecx] ptr, @[sp] ptr) None,
    Log            = __revmc_builtin_log(@[ecx] ptr, @[sp_dyn] ptr, u8) None,

    Create         = __revmc_builtin_create(@[ecx] ptr, @[sp_dyn] ptr, u8) None,
    Call           = __revmc_builtin_call(@[ecx] ptr, @[sp_dyn] ptr, u8) None,
    DoReturn       = #[NoReturn] __revmc_builtin_do_return(@[ecx] ptr, @[sp] ptr, u8) None,
    DoReturnCC     = #[NoReturn] __revmc_builtin_do_return_cc(@[ecx] ptr, usize, usize, u8) None,
    SelfDestruct   = #[NoReturn] __revmc_builtin_selfdestruct(@[ecx] ptr, @[sp] ptr) None,
}

impl Builtin {
    pub const fn call_conv(self) -> CallConv {
        match self {
            Self::Mresize => CallConv::Cold,
            _ => CallConv::Default,
        }
    }
}

impl Builtin {
    /// Resolves builtins for a concrete host type.
    pub fn addr_for<T: evm2::EvmTypesHost>(self) -> usize {
        match self {
            Self::AssertSpecId => {
                crate::generic::__revmc_builtin_assert_spec_id::<T> as *const () as usize
            }
            Self::Exp => crate::generic::__revmc_builtin_exp::<T> as *const () as usize,
            Self::Keccak256 => crate::generic::__revmc_builtin_keccak256::<T> as *const () as usize,
            Self::Keccak256CC => {
                crate::generic::__revmc_builtin_keccak256_cc::<T> as *const () as usize
            }
            Self::Balance => crate::generic::__revmc_builtin_balance::<T> as *const () as usize,
            Self::CallDataLoad => {
                crate::generic::__revmc_builtin_calldataload::<T> as *const () as usize
            }
            Self::CallDataLoadC => {
                crate::generic::__revmc_builtin_calldataload_c::<T> as *const () as usize
            }
            Self::CallDataCopy => {
                crate::generic::__revmc_builtin_calldatacopy::<T> as *const () as usize
            }
            Self::CodeCopy => crate::generic::__revmc_builtin_codecopy::<T> as *const () as usize,
            Self::ExtCodeSize => {
                crate::generic::__revmc_builtin_extcodesize::<T> as *const () as usize
            }
            Self::ExtCodeCopy => {
                crate::generic::__revmc_builtin_extcodecopy::<T> as *const () as usize
            }
            Self::ReturnDataCopy => {
                crate::generic::__revmc_builtin_returndatacopy::<T> as *const () as usize
            }
            Self::ExtCodeHash => {
                crate::generic::__revmc_builtin_extcodehash::<T> as *const () as usize
            }
            Self::BlockHash => crate::generic::__revmc_builtin_blockhash::<T> as *const () as usize,
            Self::SelfBalance => {
                crate::generic::__revmc_builtin_self_balance::<T> as *const () as usize
            }
            Self::Difficulty => {
                crate::generic::__revmc_builtin_difficulty::<T> as *const () as usize
            }
            Self::BlobHash => crate::generic::__revmc_builtin_blob_hash::<T> as *const () as usize,
            Self::Mresize => crate::generic::__revmc_builtin_mresize::<T> as *const () as usize,
            Self::Sload => crate::generic::__revmc_builtin_sload::<T> as *const () as usize,
            Self::SloadC => crate::generic::__revmc_builtin_sload_c::<T> as *const () as usize,
            Self::Sstore => crate::generic::__revmc_builtin_sstore::<T> as *const () as usize,
            Self::Tload => crate::generic::__revmc_builtin_tload::<T> as *const () as usize,
            Self::Tstore => crate::generic::__revmc_builtin_tstore::<T> as *const () as usize,
            Self::Mcopy => crate::generic::__revmc_builtin_mcopy::<T> as *const () as usize,
            Self::Log => crate::generic::__revmc_builtin_log::<T> as *const () as usize,
            Self::Create => crate::generic::__revmc_builtin_create::<T> as *const () as usize,
            Self::Call => crate::generic::__revmc_builtin_call::<T> as *const () as usize,
            Self::DoReturn => crate::generic::__revmc_builtin_do_return::<T> as *const () as usize,
            Self::DoReturnCC => {
                crate::generic::__revmc_builtin_do_return_cc::<T> as *const () as usize
            }
            Self::SelfDestruct => {
                crate::generic::__revmc_builtin_selfdestruct::<T> as *const () as usize
            }
            Self::Panic
            | Self::Div
            | Self::SDiv
            | Self::Mod
            | Self::SMod
            | Self::AddMod
            | Self::MulMod => self.addr(),
        }
    }
}
