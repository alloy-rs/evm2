#![doc = include_str!("../README.md")]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    AngleBracketedGenericArguments, FnArg, GenericArgument, GenericParam, Ident, ItemFn, LitStr,
    Pat, PatIdent, PatSlice, PathArguments, ReturnType, Stmt, Token, Type, TypeInfer, TypePath,
    TypeSlice, parse_macro_input, punctuated::Punctuated,
};

/// Generates an `Instruction` impl for an instruction function definition.
///
/// ## Parameters
///
/// - `cx: _`: Optional instruction context. It exposes `pc`, `gas`, and `state`, which are needed
///   for immediates, memory, host access, dynamic gas, active version data, and message or
///   transaction state.
/// - `[a, b]: [Word]`: Automatic stack inputs. The macro checks stack bounds, pops one word per
///   binding, and creates local `Word` values with those names.
/// - `-> out`: Automatic stack output. The output name is a mutable `Word` slot. Multiple outputs
///   can be written as `-> Result<out1, out2>` for fallible instructions.
/// - `-> Result`: Fallible instruction with no automatic outputs. The body must evaluate to
///   `evm2::interpreter::Result`, so `?` can be used to return an `InstrStop`.
/// - `-> Result<out>`: Fallible instruction with automatic outputs. The body gets mutable output
///   slots and may use `?`; no explicit final `Ok(())` is needed.
///
/// ## Attributes
///
/// - `#[instruction(no_stack_preamble)]`: Disables automatic stack input and output handling. The
///   body receives a `stack` local and is responsible for all stack reads, writes, and bounds
///   checks.
///
/// ## Examples
///
/// ### Full Signature
///
/// ```ignore
/// use evm2::interpreter::{Result, Word};
/// use evm2_macros::instruction;
///
/// #[instruction]
/// fn opcode_name(cx: _, [a, b, c]: [Word]) -> Result<out> {
///     cx.gas.spend(3)?;
///     *out = a.wrapping_add(b).wrapping_add(c);
/// }
/// ```
///
/// ### Stack only
///
/// Stack-only instructions can omit `cx: _` and `Result`:
///
/// ```ignore
/// use evm2::interpreter::Word;
/// use evm2_macros::instruction;
///
/// #[instruction]
/// fn add([a, b]: [Word]) -> out {
///     *out = a.wrapping_add(b);
/// }
/// ```
///
/// ### No stack preamble
///
/// ```ignore
/// use evm2::interpreter::{Result, Word};
/// use evm2_macros::instruction;
///
/// #[instruction(no_stack_preamble)]
/// fn no_stack_preamble_opcode(cx: _) -> Result {
///     cx.gas.spend(2)?;
///     stack.push(Word::ZERO)
/// }
/// ```
#[proc_macro_attribute]
pub fn instruction(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr with Punctuated::<Ident, Token![,]>::parse_terminated);
    let no_stack_preamble = match args.len() {
        0 => false,
        1 if args[0] == "no_stack_preamble" => true,
        _ => {
            return syn::Error::new_spanned(
                args,
                "expected `#[instruction]` or `#[instruction(no_stack_preamble)]`",
            )
            .to_compile_error()
            .into();
        }
    };
    let input = parse_macro_input!(item as ItemFn);
    expand_instruction(no_stack_preamble, input).into()
}

fn expand_instruction(no_stack_preamble: bool, input: ItemFn) -> TokenStream2 {
    let attrs = input.attrs;
    let vis = input.vis;
    let sig = input.sig;
    let ident = sig.ident;
    let asm_comment = LitStr::new(&ident.to_string(), ident.span());
    let generics = sig.generics;
    let struct_where_clause = generics.where_clause.clone();
    let impl_params = generics.params.clone();
    let evm_types = Ident::new("__Evm2T", ident.span());
    let struct_generics = if impl_params.is_empty() {
        quote! { <#evm_types: evm2::EvmTypes> }
    } else {
        quote! { <#evm_types: evm2::EvmTypes, #impl_params> }
    };
    let type_params = generics.params.iter().map(generic_param_ident);
    let type_generics = quote! { <#evm_types #(, #type_params)*> };
    let where_predicates =
        struct_where_clause.as_ref().map(|where_clause| &where_clause.predicates);
    let impl_where_clause = where_predicates.map(|predicates| quote! { where #predicates });
    let (_, outputs) = parse_return(sig.output);
    let body = body(input.block.stmts, outputs.is_empty());

    let mut has_cx = false;
    let mut cx_arg = None;
    let mut inputs = Vec::new();
    for arg in sig.inputs {
        let FnArg::Typed(arg) = arg else { continue };
        if is_infer(&arg.ty) {
            let Pat::Ident(PatIdent { ident, .. }) = *arg.pat else { continue };
            has_cx = true;
            cx_arg = Some(ident);
        } else if is_word_slice(&arg.ty) {
            let Pat::Slice(PatSlice { elems, .. }) = *arg.pat else { continue };
            inputs.extend(elems.into_iter().filter_map(|pat| {
                let Pat::Ident(PatIdent { ident, .. }) = pat else {
                    return None;
                };
                Some(ident)
            }));
        } else {
            return syn::Error::new_spanned(
                arg,
                "unsupported #[instruction] argument; use `cx: _` for context or `[a, b]: [Word]` for stack inputs",
            )
            .to_compile_error();
        }
    }

    let stack_setup = (!no_stack_preamble).then(|| stack_setup(&inputs, &outputs));
    let cx_setup = has_cx.then(|| {
        let cx = cx_arg.unwrap_or_else(|| Ident::new("cx", ident.span()));
        quote! {
            let mut __evm2_gas_cx = evm2::interpreter::GasCx::new(
                __evm2_remaining_gas,
                __evm2_gas,
            );
            let mut #cx = evm2::interpreter::InstructionCx::<#evm_types> {
                pc: __evm2_pc,
                gas: &mut __evm2_gas_cx,
                state: __evm2_state,
            };
        }
    });
    quote! {
        #(#attrs)*
        #[allow(non_camel_case_types)]
        #vis struct #ident #struct_generics(
            core::marker::PhantomData<fn() -> #evm_types>
        ) #struct_where_clause;

        impl #struct_generics evm2::interpreter::Instruction<#evm_types> for #ident #type_generics
        #impl_where_clause
        {
            #[inline]
            fn execute(
                __evm2_pc: &mut evm2::interpreter::Pc,
                mut stack: evm2::interpreter::StackMut<'_>,
                __evm2_remaining_gas: &mut evm2::interpreter::RemainingGas,
                __evm2_gas: &mut evm2::interpreter::Gas,
                __evm2_state: &mut evm2::interpreter::State<'_, #evm_types>,
            ) -> evm2::interpreter::Result {
                evm2::asm_comment!(#asm_comment);
                #cx_setup
                #stack_setup
                #body
            }
        }
    }
}

fn generic_param_ident(param: &GenericParam) -> TokenStream2 {
    match param {
        GenericParam::Type(param) => {
            let ident = &param.ident;
            quote! { #ident }
        }
        GenericParam::Lifetime(param) => {
            let lifetime = &param.lifetime;
            quote! { #lifetime }
        }
        GenericParam::Const(param) => {
            let ident = &param.ident;
            quote! { #ident }
        }
    }
}

const fn is_infer(ty: &Type) -> bool {
    matches!(ty, Type::Infer(TypeInfer { .. }))
}

fn is_word_slice(ty: &Type) -> bool {
    let Type::Slice(TypeSlice { elem, .. }) = ty else {
        return false;
    };
    let Type::Path(TypePath { path, .. }) = &**elem else {
        return false;
    };
    path.get_ident().is_some_and(|ident| ident == "Word")
}

fn parse_return(output: ReturnType) -> (bool, Vec<Ident>) {
    let ReturnType::Type(_, ty) = output else {
        return (false, Vec::new());
    };
    let Type::Path(TypePath { path, .. }) = *ty else {
        return (false, Vec::new());
    };
    let Some(segment) = path.segments.last() else {
        return (false, Vec::new());
    };
    if segment.ident == "Result" {
        let outputs = match &segment.arguments {
            PathArguments::AngleBracketed(AngleBracketedGenericArguments { args, .. }) => {
                args.iter().filter_map(generic_ident).collect()
            }
            _ => Vec::new(),
        };
        (true, outputs)
    } else {
        (false, vec![segment.ident.clone()])
    }
}

fn generic_ident(arg: &GenericArgument) -> Option<Ident> {
    let GenericArgument::Type(Type::Path(TypePath { path, .. })) = arg else {
        return None;
    };
    path.get_ident().cloned()
}

fn body(stmts: Vec<Stmt>, allow_final_result: bool) -> TokenStream2 {
    if allow_final_result && matches!(stmts.last(), Some(Stmt::Expr(_, None))) {
        quote! { #(#stmts)* }
    } else {
        let stmts = stmts.into_iter().map(|stmt| match stmt {
            Stmt::Expr(expr, None) => quote! { #expr; },
            stmt => quote! { #stmt },
        });
        quote! {
            #(#stmts)*
            Ok(())
        }
    }
}

fn stack_setup(inputs: &[Ident], outputs: &[Ident]) -> TokenStream2 {
    let input_count = inputs.len();
    let input_setup = (input_count > 0).then(|| {
        let input_bindings = inputs.iter().rev();
        quote! {
            let [#(#input_bindings),*] = unsafe { ptr.cast::<[Word; #input_count]>().read() };
        }
    });

    let output_count = outputs.len();
    let output_setup = (output_count > 0).then(|| {
        let output_bindings = outputs.iter().rev();
        quote! {
            let [#(#output_bindings),*] = unsafe { &mut *ptr.cast::<[Word; #output_count]>() };
        }
    });

    quote! {
        let ptr = stack.instr_stack_setup(#input_count, #output_count)?;
        #input_setup
        #output_setup
    }
}
