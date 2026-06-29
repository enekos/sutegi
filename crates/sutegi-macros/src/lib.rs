//! `#[derive(Model)]` for sutegi.
//!
//! Generates, for a plain struct with named fields:
//! * `impl sutegi::orm::Model` — the table schema (column types, primary key)
//! * `impl sutegi::orm::row::FromRow` — typed hydration from a JSON row
//! * inherent `to_values(&self)` — `(column, Value)` pairs for inserts
//! * inherent `to_json(&self)` — a clean JSON object (bools as real bools)
//!
//! Generated paths go through the `sutegi` facade crate, so depend on `sutegi`
//! (the normal entry point). This crate's own dependencies (syn/quote) are
//! compile-time only and never reach your binary.
//!
//! ```ignore
//! #[derive(Model)]
//! #[model(table = "todos")]
//! struct Todo {
//!     #[model(primary)]
//!     id: i64,
//!     title: String,
//!     done: bool,
//!     note: Option<String>,
//! }
//! ```

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Fields, Type};

#[derive(Clone, Copy)]
enum Scalar {
    Int,
    Real,
    Text,
    Bool,
}

struct FieldInfo {
    ident: syn::Ident,
    column: String,
    scalar: Scalar,
    optional: bool,
    primary: bool,
}

#[proc_macro_derive(Model, attributes(model))]
pub fn derive_model(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

fn expand(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let name = &input.ident;

    // Struct-level: #[model(table = "...")]
    let mut table: Option<String> = None;
    for attr in &input.attrs {
        if attr.path().is_ident("model") {
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("table") {
                    let lit: syn::LitStr = meta.value()?.parse()?;
                    table = Some(lit.value());
                    Ok(())
                } else {
                    Err(meta.error("unknown #[model(...)] key on struct (expected `table`)"))
                }
            })?;
        }
    }
    let table = table.ok_or_else(|| {
        syn::Error::new_spanned(&input, "missing #[model(table = \"...\")] on the struct")
    })?;

    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    &input,
                    "#[derive(Model)] requires named fields",
                ))
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                &input,
                "#[derive(Model)] can only be applied to structs",
            ))
        }
    };

    let mut infos = Vec::new();
    for field in fields {
        let ident = field.ident.clone().unwrap();
        let mut column = ident.to_string();
        let mut primary = false;

        for attr in &field.attrs {
            if attr.path().is_ident("model") {
                attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("primary") {
                        primary = true;
                        Ok(())
                    } else if meta.path.is_ident("column") {
                        let lit: syn::LitStr = meta.value()?.parse()?;
                        column = lit.value();
                        Ok(())
                    } else {
                        Err(meta.error("unknown #[model(...)] key on field (expected `primary` or `column`)"))
                    }
                })?;
            }
        }

        let (scalar, optional) = classify(&field.ty).ok_or_else(|| {
            syn::Error::new_spanned(
                &field.ty,
                "unsupported field type for #[derive(Model)] (use i64/i32, f64/f32, String, bool, or Option<…> of those)",
            )
        })?;

        infos.push(FieldInfo { ident, column, scalar, optional, primary });
    }

    // ---- schema() columns ----
    let column_defs = infos.iter().map(|f| {
        let col = &f.column;
        let ty = match f.scalar {
            Scalar::Int => quote!(::sutegi::orm::ColType::Integer),
            Scalar::Real => quote!(::sutegi::orm::ColType::Real),
            Scalar::Text => quote!(::sutegi::orm::ColType::Text),
            Scalar::Bool => quote!(::sutegi::orm::ColType::Boolean),
        };
        let nullable = f.optional;
        let primary = f.primary;
        quote! {
            ::sutegi::orm::Column { name: #col, ty: #ty, nullable: #nullable, primary: #primary }
        }
    });

    // ---- from_row() field initializers ----
    let from_row_inits = infos.iter().map(|f| {
        let ident = &f.ident;
        let col = &f.column;
        let extractor = match (f.scalar, f.optional) {
            (Scalar::Int, false) => quote!(::sutegi::orm::row::get_i64),
            (Scalar::Real, false) => quote!(::sutegi::orm::row::get_f64),
            (Scalar::Text, false) => quote!(::sutegi::orm::row::get_string),
            (Scalar::Bool, false) => quote!(::sutegi::orm::row::get_bool),
            (Scalar::Int, true) => quote!(::sutegi::orm::row::opt_i64),
            (Scalar::Real, true) => quote!(::sutegi::orm::row::opt_f64),
            (Scalar::Text, true) => quote!(::sutegi::orm::row::opt_string),
            (Scalar::Bool, true) => quote!(::sutegi::orm::row::opt_bool),
        };
        // Int fields may be narrower than i64; cast on the way in.
        let cast = matches!(f.scalar, Scalar::Int | Scalar::Real);
        if cast && !f.optional {
            quote! { #ident: #extractor(row, #col)? as _ }
        } else if cast && f.optional {
            quote! { #ident: #extractor(row, #col)?.map(|v| v as _) }
        } else {
            quote! { #ident: #extractor(row, #col)? }
        }
    });

    // ---- to_values() pairs ----
    let to_values = infos.iter().map(|f| {
        let ident = &f.ident;
        let col = &f.column;
        let wrap = value_wrapper(f.scalar);
        if f.optional {
            quote! {
                (#col, match &self.#ident {
                    ::core::option::Option::Some(v) => (#wrap)(v.clone()),
                    ::core::option::Option::None => ::sutegi::orm::Value::Null,
                })
            }
        } else {
            quote! { (#col, (#wrap)(self.#ident.clone())) }
        }
    });

    // ---- to_json() pairs ----
    let to_json = infos.iter().map(|f| {
        let ident = &f.ident;
        let col = &f.column;
        let to = json_wrapper(f.scalar);
        if f.optional {
            quote! {
                (#col, match &self.#ident {
                    ::core::option::Option::Some(v) => (#to)(v.clone()),
                    ::core::option::Option::None => ::sutegi::json::Json::Null,
                })
            }
        } else {
            quote! { (#col, (#to)(self.#ident.clone())) }
        }
    });

    Ok(quote! {
        impl ::sutegi::orm::Model for #name {
            fn schema() -> ::sutegi::orm::TableSchema {
                ::sutegi::orm::TableSchema {
                    table: #table,
                    columns: ::std::vec![ #( #column_defs ),* ],
                }
            }
        }

        impl ::sutegi::orm::row::FromRow for #name {
            fn from_row(row: &::sutegi::json::Json) -> ::core::result::Result<Self, ::std::string::String> {
                ::core::result::Result::Ok(Self { #( #from_row_inits ),* })
            }
        }

        impl #name {
            /// `(column, Value)` pairs for inserts.
            pub fn to_values(&self) -> ::std::vec::Vec<(&'static str, ::sutegi::orm::Value)> {
                ::std::vec![ #( #to_values ),* ]
            }

            /// Serialize to a JSON object (booleans render as real booleans).
            pub fn to_json(&self) -> ::sutegi::json::Json {
                ::sutegi::json::Json::obj(::std::vec![ #( #to_json ),* ])
            }
        }
    })
}

/// Map a Rust field type to (scalar kind, is_option). Returns None if unsupported.
fn classify(ty: &Type) -> Option<(Scalar, bool)> {
    let seg = last_segment(ty)?;
    if seg == "Option" {
        let inner = option_inner(ty)?;
        let inner_seg = last_segment(inner)?;
        scalar_of(&inner_seg).map(|s| (s, true))
    } else {
        scalar_of(&seg).map(|s| (s, false))
    }
}

fn scalar_of(name: &str) -> Option<Scalar> {
    match name {
        "i64" | "i32" | "i16" | "i8" | "u64" | "u32" | "u16" | "u8" | "usize" | "isize" => {
            Some(Scalar::Int)
        }
        "f64" | "f32" => Some(Scalar::Real),
        "String" => Some(Scalar::Text),
        "bool" => Some(Scalar::Bool),
        _ => None,
    }
}

fn value_wrapper(scalar: Scalar) -> proc_macro2::TokenStream {
    match scalar {
        Scalar::Int => quote!(|v: _| ::sutegi::orm::Value::Int(v as i64)),
        Scalar::Real => quote!(|v: _| ::sutegi::orm::Value::Real(v as f64)),
        Scalar::Text => quote!(|v: ::std::string::String| ::sutegi::orm::Value::Text(v)),
        Scalar::Bool => quote!(|v: bool| ::sutegi::orm::Value::Bool(v)),
    }
}

fn json_wrapper(scalar: Scalar) -> proc_macro2::TokenStream {
    match scalar {
        Scalar::Int => quote!(|v: _| ::sutegi::json::Json::int(v as i64)),
        Scalar::Real => quote!(|v: _| ::sutegi::json::Json::Num(v as f64)),
        Scalar::Text => quote!(|v: ::std::string::String| ::sutegi::json::Json::Str(v)),
        Scalar::Bool => quote!(|v: bool| ::sutegi::json::Json::Bool(v)),
    }
}

fn last_segment(ty: &Type) -> Option<String> {
    if let Type::Path(p) = ty {
        p.path.segments.last().map(|s| s.ident.to_string())
    } else {
        None
    }
}

fn option_inner(ty: &Type) -> Option<&Type> {
    if let Type::Path(p) = ty {
        let seg = p.path.segments.last()?;
        if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
            if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                return Some(inner);
            }
        }
    }
    None
}
