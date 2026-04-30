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
    let (_, outputs) = parse_return(sig.output);
    let body = body(input.block.stmts, outputs.is_empty());

    let mut has_cx = false;
    let mut cx_arg = None;
    let mut inputs = Vec::new();
    for arg in sig.inputs {
        let FnArg::Typed(arg) = arg else { continue };
        let Pat::Ident(PatIdent { ident, .. }) = *arg.pat else { continue };
        if is_infer(&arg.ty) {
            has_cx = true;
            cx_arg = Some(ident);
        } else {
            inputs.push(ident);
        }
    }

    let stack_setup = (!raw).then(|| stack_setup(&inputs, &outputs));
    let cx_setup = has_cx.then(|| {
        let cx = cx_arg.unwrap_or_else(|| Ident::new("cx", ident.span()));
        quote! {
            let mut ctrl = ctrl;
            let mut #cx = InstructionCx { ctrl: &mut ctrl, gas, state };
        }
    });
    quote! {
        #(#attrs)*
        #[inline]
        #vis fn #ident #generics(
            mut ctrl: CtrlRef<'_>,
            stack: &mut Stack<'_>,
            gas: &mut Gas,
            state: &mut State<'_>,
        ) -> Result
        #where_clause
        {
            #cx_setup
            #stack_setup
            #body
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
        quote! {
            let [#(#inputs),*] = unsafe { ptr.cast::<[Word; #input_count]>().read() };
            #(let #inputs = &#inputs;)*
        }
    });

    match outputs {
        [] if input_count == 0 => quote! {},
        [] => {
            let len_update = decrease_len(input_count);
            quote! {
                stack.check_bounds(#input_count, 0)?;
                let ptr = unsafe { stack.stack.as_mut_ptr().add(stack.len).sub(#input_count) };
                #input_setup
                #len_update
            }
        }
        [output] => {
            let len_update = match input_count {
                0 => quote! { stack.len += 1; },
                1 => quote! {},
                _ => decrease_len(input_count - 1),
            };
            quote! {
                stack.check_bounds(#input_count, 1)?;
                let ptr = unsafe { stack.stack.as_mut_ptr().add(stack.len).sub(#input_count) };
                #input_setup
                let #output = unsafe { &mut *ptr.cast::<Word>() };
                #len_update
            }
        }
        _ => quote! {
            compile_error!("multiple instruction outputs are not supported yet");
        },
    }
}

fn decrease_len(amount: usize) -> TokenStream2 {
    if amount == 0 {
        quote! {}
    } else {
        quote! { stack.len -= #amount; }
    }
}
