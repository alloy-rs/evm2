#![doc = include_str!("../README.md")]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    AngleBracketedGenericArguments, FnArg, GenericArgument, Ident, ItemFn, LitStr, Pat, PatIdent,
    PatSlice, PathArguments, ReturnType, Stmt, Token, Type, TypeInfer, TypePath, TypeSlice,
    parse_macro_input, punctuated::Punctuated,
};

/// Lowers instruction functions into the interpreter ABI.
#[proc_macro_attribute]
pub fn instruction(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr with Punctuated::<Ident, Token![,]>::parse_terminated);
    let raw = args.iter().any(|arg| arg == "raw");
    let input = parse_macro_input!(item as ItemFn);
    expand_instruction(raw, input).into()
}

fn expand_instruction(raw: bool, input: ItemFn) -> TokenStream2 {
    let attrs = input.attrs;
    let vis = input.vis;
    let sig = input.sig;
    let ident = sig.ident;
    let asm_comment = LitStr::new(&ident.to_string(), ident.span());
    let generics = sig.generics;
    let struct_where_clause = generics.where_clause.clone();
    let impl_params = generics.params.clone();
    let impl_generics = if impl_params.is_empty() {
        quote! { <C: evm2::EvmConfig> }
    } else {
        quote! { <C: evm2::EvmConfig, #impl_params> }
    };
    let (_, type_generics, _) = generics.split_for_impl();
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
            let Pat::Ident(PatIdent { ident, .. }) = *arg.pat else { continue };
            inputs.push(ident);
        }
    }

    let stack_setup = (!raw).then(|| stack_setup(&inputs, &outputs));
    let cx_setup = has_cx.then(|| {
        let cx = cx_arg.unwrap_or_else(|| Ident::new("cx", ident.span()));
        quote! {
            let mut #cx: evm2::interpreter::table::InstructionCx<'_, '_, C> = evm2::interpreter::table::InstructionCx {
                pc: __evm2_pc,
                gas: __evm2_gas,
                gas_params: const { &C::GAS_PARAMS },
                state: __evm2_state,
            };
        }
    });
    quote! {
        #(#attrs)*
        #[allow(non_camel_case_types)]
        #vis struct #ident #generics #struct_where_clause;

        impl #impl_generics evm2::interpreter::table::Instruction<C> for #ident #type_generics
        #impl_where_clause
        {
            #[inline]
            fn execute(
                &self,
                __evm2_pc: &mut evm2::interpreter::Pc,
                mut stack: evm2::interpreter::StackMut<'_>,
                __evm2_gas: &mut evm2::interpreter::Gas,
                __evm2_state: &mut evm2::interpreter::State<'_, <C as evm2::EvmConfig>::Host>,
            ) -> evm2::interpreter::Result {
                evm2::asm_comment!(#asm_comment);
                #cx_setup
                #stack_setup
                #body
            }
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
