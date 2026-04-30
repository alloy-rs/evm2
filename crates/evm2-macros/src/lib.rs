use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    AngleBracketedGenericArguments, FnArg, GenericArgument, Ident, ItemFn, Pat, PatIdent,
    PathArguments, ReturnType, Stmt, Token, Type, TypeInfer, TypePath, parse_macro_input,
    punctuated::Punctuated,
};

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
    let generics = sig.generics;
    let where_clause = generics.where_clause.clone();
    let body = input.block.stmts;

    let mut has_cx = false;
    let mut inputs = Vec::new();
    for arg in sig.inputs {
        let FnArg::Typed(arg) = arg else { continue };
        let Pat::Ident(PatIdent { ident, .. }) = *arg.pat else { continue };
        if is_infer(&arg.ty) {
            has_cx = true;
        } else {
            inputs.push(ident);
        }
    }

    let (_, outputs) = parse_return(sig.output);
    let stack_setup = (!raw).then(|| stack_setup(&inputs, &outputs));
    let needs_cx = raw || has_cx || stack_setup.as_ref().is_some_and(|setup| !setup.is_empty());
    let cx_setup = needs_cx.then(|| {
        quote! {
            let mut cx =
                InstructionCx { pc: &mut pc, stack: &mut stack, gas, host: &mut *state.host };
        }
    });
    let body = body_with_semicolon(body);
    let setup = if raw {
        quote! {
            #cx_setup
        }
    } else {
        quote! {
            #cx_setup
            #stack_setup
        }
    };

    quote! {
        extern_table! {
            #(#attrs)*
            #[inline]
            #[allow(unreachable_code)]
            #vis fn #ident #generics(
                mut pc: PcRef<'_>,
                mut stack: Stack<'_>,
                gas: GasRef<'_>,
                state: &mut State<'_>,
            ) -> InstrFnRet
            #where_clause
            {
                let r = (|| -> Result {
                    #setup
                    #(#body)*
                    Ok(())
                })();
                (stack.len, r)
            }
        }
    }
}

fn is_infer(ty: &Type) -> bool {
    matches!(ty, Type::Infer(TypeInfer { .. }))
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

fn stack_setup(inputs: &[Ident], outputs: &[Ident]) -> TokenStream2 {
    let input_count = inputs.len();
    let output_count = outputs.len();
    let input_setup = (input_count > 0).then(|| {
        quote! {
            let [#(#inputs),*] = unsafe { ptr.cast::<[Word; IN]>().read() };
            #(let #inputs = &#inputs;)*
        }
    });
    match outputs {
        [] if input_count == 0 => quote! {},
        [] => {
            quote! {
                const IN: usize = #input_count;
                const OUT: usize = #output_count;
                const REQUIRED_LEN: usize = if IN > OUT { IN } else { OUT };
                if cx.stack.len < REQUIRED_LEN {
                    cold_path();
                    return Err(InstrErr::StackUnderflow);
                }
                let ptr = unsafe { cx.stack.stack.as_mut_ptr().add(cx.stack.len).sub(IN) };
                #input_setup
                cx.stack.len -= IN;
            }
        }
        [output] => {
            quote! {
                const IN: usize = #input_count;
                const OUT: usize = #output_count;
                const REQUIRED_LEN: usize = if IN > OUT { IN } else { OUT };
                if IN != 0 && cx.stack.len < REQUIRED_LEN {
                    cold_path();
                    return Err(InstrErr::StackUnderflow);
                }
                let new_len = cx.stack.len - IN + OUT;
                if new_len > 1024 {
                    cold_path();
                    return Err(InstrErr::StackOverflow);
                }
                let ptr = unsafe { cx.stack.stack.as_mut_ptr().add(cx.stack.len).sub(IN) };
                #input_setup
                let #output = unsafe { &mut *ptr.cast::<Word>() };
                cx.stack.len = new_len;
            }
        }
        _ => quote! {
            compile_error!("multiple instruction outputs are not supported yet");
        },
    }
}

fn body_with_semicolon(stmts: Vec<Stmt>) -> Vec<TokenStream2> {
    stmts
        .into_iter()
        .map(|stmt| match stmt {
            Stmt::Expr(expr, None) => quote! { #expr; },
            stmt => quote! { #stmt },
        })
        .collect()
}
