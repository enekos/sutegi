use proc_macro2::Span;
use syn::spanned::Spanned;
use syn::Attribute;

pub(crate) enum Kind {
    Required,
    Str,
    Integer,
    Number,
    Bool,
    Email,
    Url,
    Alpha,
    AlphaNum,
    MinLen(usize),
    MaxLen(usize),
    Min(f64),
    Max(f64),
    Same(String),
}

pub(crate) struct RuleAttr {
    pub(crate) kind: Kind,
    pub(crate) span: Span,
}

pub(crate) fn parse(attrs: &[Attribute]) -> syn::Result<Vec<RuleAttr>> {
    let mut out = Vec::new();
    for attr in attrs.iter().filter(|a| a.path().is_ident("validate")) {
        attr.parse_nested_meta(|meta| {
            let span = meta.path.span();
            let name = meta
                .path
                .get_ident()
                .map(|i| i.to_string())
                .unwrap_or_default();
            let kind = match name.as_str() {
                "required" => Kind::Required,
                "str" => Kind::Str,
                "integer" => Kind::Integer,
                "number" => Kind::Number,
                "bool" => Kind::Bool,
                "email" => Kind::Email,
                "url" => Kind::Url,
                "alpha" => Kind::Alpha,
                "alphanum" => Kind::AlphaNum,
                "min_len" => Kind::MinLen(meta.value()?.parse::<syn::LitInt>()?.base10_parse()?),
                "max_len" => Kind::MaxLen(meta.value()?.parse::<syn::LitInt>()?.base10_parse()?),
                "min" => Kind::Min(number(&meta.value()?.parse()?)?),
                "max" => Kind::Max(number(&meta.value()?.parse()?)?),
                "same" => Kind::Same(meta.value()?.parse::<syn::LitStr>()?.value()),
                _ => return Err(meta.error("unknown #[validate(...)] rule")),
            };
            out.push(RuleAttr { kind, span });
            Ok(())
        })?;
    }
    Ok(out)
}

fn number(expr: &syn::Expr) -> syn::Result<f64> {
    match expr {
        syn::Expr::Lit(syn::ExprLit { lit, .. }) => match lit {
            syn::Lit::Int(i) => i.base10_parse::<f64>(),
            syn::Lit::Float(f) => f.base10_parse::<f64>(),
            other => Err(syn::Error::new_spanned(other, "expected a number")),
        },
        syn::Expr::Unary(syn::ExprUnary {
            op: syn::UnOp::Neg(_),
            expr,
            ..
        }) => number(expr).map(|n| -n),
        other => Err(syn::Error::new_spanned(other, "expected a number")),
    }
}
