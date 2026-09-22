//! Declarative state-machine syntax for Reflex.
use proc_macro::TokenStream;
use quote::quote;
use syn::{
    braced, bracketed, parenthesized,
    parse::{Parse, ParseStream},
    parse_macro_input,
    punctuated::Punctuated,
    Expr, Ident, Pat, Path, Token, Type,
};
struct Declaration {
    phase: Type,
    data: Type,
    action: Type,
    event: Type,
    invariants: Vec<Expr>,
    no_change: Option<Pat>,
    transitions: Vec<Row>,
}
struct Row {
    from: Path,
    kind: Ident,
    pattern: Pat,
    to: Path,
    hooks: Vec<(Ident, Expr)>,
}
impl Parse for Row {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let from = input.parse()?;
        input.parse::<Token![+]>()?;
        let kind: Ident = input.parse()?;
        if !["action", "event", "evaluation_error"].contains(&kind.to_string().as_str()) {
            return Err(syn::Error::new_spanned(
                kind,
                "expected action, event, or evaluation_error",
            ));
        }
        let content;
        parenthesized!(content in input);
        let pattern = content.call(Pat::parse_multi_with_leading_vert)?;
        if !content.is_empty() {
            return Err(content.error("expected one input pattern"));
        }
        input.parse::<Token![=>]>()?;
        let to = input.parse()?;
        let content;
        braced!(content in input);
        let mut hooks = Vec::new();
        while !content.is_empty() {
            let name: Ident = content.parse()?;
            if !["guard", "update", "effect", "min_confidence"].contains(&name.to_string().as_str())
            {
                return Err(syn::Error::new_spanned(name, "unknown transition field"));
            }
            if hooks.iter().any(|(key, _)| *key == name) {
                return Err(syn::Error::new_spanned(name, "duplicate transition field"));
            }
            content.parse::<Token![:]>()?;
            hooks.push((name, content.parse()?));
            if !content.is_empty() {
                content.parse::<Token![,]>()?;
            }
        }
        Ok(Self {
            from,
            kind,
            pattern,
            to,
            hooks,
        })
    }
}
impl Parse for Declaration {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let (mut phase, mut data, mut action, mut event) = (None, None, None, None);
        let (mut invariants, mut no_change, mut transitions) = (None, None, None);
        let mut names = std::collections::HashSet::new();
        while !input.is_empty() {
            let key: Ident = input.parse()?;
            if !names.insert(key.to_string()) {
                return Err(syn::Error::new_spanned(key, "duplicate definition field"));
            }
            input.parse::<Token![:]>()?;
            match key.to_string().as_str() {
                "phase" => phase = Some(input.parse()?),
                "data" => data = Some(input.parse()?),
                "action" => action = Some(input.parse()?),
                "event" => event = Some(input.parse()?),
                "no_change" => no_change = Some(input.call(Pat::parse_multi_with_leading_vert)?),
                "invariants" => {
                    let content;
                    bracketed!(content in input);
                    invariants = Some(
                        Punctuated::<Expr, Token![,]>::parse_terminated(&content)?
                            .into_iter()
                            .collect(),
                    );
                }
                "transitions" => {
                    let content;
                    bracketed!(content in input);
                    transitions = Some(
                        Punctuated::<Row, Token![,]>::parse_terminated(&content)?
                            .into_iter()
                            .collect(),
                    );
                }
                _ => return Err(syn::Error::new_spanned(key, "unknown definition field")),
            }
            if !input.is_empty() {
                input.parse::<Token![,]>()?;
            }
        }
        Ok(Self {
            phase: phase.ok_or_else(|| input.error("phase type is required"))?,
            data: data.ok_or_else(|| input.error("data type is required"))?,
            action: action.ok_or_else(|| input.error("action type is required"))?,
            event: event
                .ok_or_else(|| input.error("event type is required (use () for no events)"))?,
            invariants: invariants.unwrap_or_default(),
            no_change,
            transitions: transitions.ok_or_else(|| input.error("transitions are required"))?,
        })
    }
}
fn expand(d: Declaration) -> syn::Result<proc_macro2::TokenStream> {
    let Declaration {
        phase,
        data,
        action,
        event,
        invariants,
        no_change,
        transitions,
    } = d;
    let no_change = no_change.map_or(
        quote!(None),
        |p| quote!(Some(Box::new(|value: &#action| matches!(value, #p)))),
    );
    let mut rows = Vec::new();
    for Row {
        from,
        kind,
        pattern,
        to,
        hooks,
    } in transitions
    {
        let get = |name: &str| {
            hooks
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value)
        };
        let unchanged = to.is_ident("unchanged");
        let (guard, update, effect, minimum) = (
            get("guard"),
            get("update"),
            get("effect"),
            get("min_confidence"),
        );
        if unchanged && (update.is_some() || effect.is_some()) {
            return Err(syn::Error::new_spanned(
                to,
                "unchanged rows cannot update data or dispatch effects",
            ));
        }
        if effect.is_some() && update.is_none() {
            return Err(syn::Error::new_spanned(
                to,
                "an effect requires a payload-producing update",
            ));
        }
        if minimum.is_some() && kind != "action" {
            return Err(syn::Error::new_spanned(
                kind,
                "min_confidence is valid only on action rows",
            ));
        }
        let (variant, input_type, extract) = match kind.to_string().as_str() {
            "action" => (quote!(Action), quote!(#action), quote!(decision.action())),
            "event" => (quote!(Event), quote!(#event), quote!(decision)),
            _ => (
                quote!(EvaluationError),
                quote!(::reflex::EvaluationError),
                quote!(decision),
            ),
        };
        let target = if unchanged {
            quote!(None)
        } else {
            quote!(Some(#to))
        };
        let threshold = minimum.map_or(quote!(None), |v| quote!(Some(#v)));
        let bind_guard = guard.map(|g| quote!(let guard_hook = #g;));
        let bind_update = update.map(|u| quote!(let update_hook = #u;));
        let bind_effect = effect.map(|e| quote!(let effect_hook = ::std::sync::Arc::new(#e);));
        let guard_body = if guard.is_some() {
            quote!(guard_hook(data, value, now))
        } else {
            quote!(Ok(()))
        };
        let update_body = match (update, effect) {
            (Some(_), Some(_)) => quote! {
                let payload = update_hook(data, value, now)?;
                let handler = effect_hook.clone();
                let future: ::reflex::EffectFuture<#event> = Box::pin(async move { handler(payload).await });
                Ok(Some(future))
            },
            (Some(_), None) => quote! { let _: () = update_hook(data, value, now)?; Ok(None) },
            _ => quote!(Ok(None)),
        };
        rows.push(quote! {{
            #bind_guard
            #bind_update
            #bind_effect
            ::reflex::Transition {
                from: #from, to: #target, kind: ::reflex::InputKind::#variant, min_confidence: #threshold,
                matches: Box::new(|input| match input {
                    ::reflex::MachineInput::#variant(decision) => { let value: &#input_type = #extract; matches!(value, #pattern) }, _ => false,
                }),
                guard: Box::new(move |data, input, now| {
                    let ::reflex::MachineInput::#variant(decision) = input else { unreachable!("matched row input kind") };
                    let value: &#input_type = #extract; let _ = (&data, &value, &now); #guard_body
                }),
                update: Box::new(move |data, input, now| {
                    let ::reflex::MachineInput::#variant(decision) = input else { unreachable!("matched row input kind") };
                    let value: &#input_type = #extract; let _ = (&data, &value, &now); #update_body
                }),
            }
        }});
    }
    Ok(
        quote! { ::reflex::MachineDefinition::<#phase, #data, #action, #event> {
            invariants: vec![#(Box::new(#invariants)),*], no_change: #no_change, transitions: vec![#(#rows),*],
        } },
    )
}
#[proc_macro]
pub fn state_machine(input: TokenStream) -> TokenStream {
    expand(parse_macro_input!(input as Declaration))
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
