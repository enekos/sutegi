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
    /// A `Json` field ⇒ a `json`/`jsonb` (or SQLite `TEXT`) column.
    Json,
    /// A `Vec<f32>` field marked `#[model(vector)]` ⇒ a `vector` column.
    Vector,
}

/// The kind of association a relation field represents.
#[derive(Clone, Copy, PartialEq)]
enum RelKind {
    /// This model has many of the related model (child holds the foreign key).
    HasMany,
    /// This model has exactly one of the related model (child holds the FK).
    HasOne,
    /// This model belongs to the related model (this model holds the FK).
    BelongsTo,
}

/// A parsed `#[model(has_many(...) / has_one(...) / belongs_to(...))]`.
struct RelationInfo {
    kind: RelKind,
    /// The related model type (e.g. `Post`).
    related: syn::Path,
    /// The foreign-key **column** name. On has_*, it lives on the child; on
    /// belongs_to, it is a column on this model.
    foreign_key: String,
    /// Optional join-key override: the local key column for has_* (defaults to
    /// this model's primary key), or the owner key column for belongs_to
    /// (defaults to the related model's primary key).
    key: Option<String>,
}

struct FieldInfo {
    ident: syn::Ident,
    column: String,
    scalar: Scalar,
    optional: bool,
    primary: bool,
    /// Not a column: excluded from schema/persistence, default-initialized on load.
    skip: bool,
    /// Set when the field is an eager-loadable relation rather than a column.
    relation: Option<RelationInfo>,
    /// For `Scalar::Vector`: the fixed embedding dimension, if declared.
    vector_dim: Option<usize>,
}

impl FieldInfo {
    /// A real table column — persisted, part of the schema, hydrated from a row.
    /// (Skipped fields and relations are neither.)
    fn is_column(&self) -> bool {
        !self.skip && self.relation.is_none()
    }
}

/// Map a relation attribute path to its kind.
fn rel_kind(path: &syn::Path) -> Option<RelKind> {
    if path.is_ident("has_many") {
        Some(RelKind::HasMany)
    } else if path.is_ident("has_one") {
        Some(RelKind::HasOne)
    } else if path.is_ident("belongs_to") {
        Some(RelKind::BelongsTo)
    } else {
        None
    }
}

#[proc_macro_derive(Model, attributes(model))]
pub fn derive_model(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// `#[derive(Validate)]` — generate `impl sutegi::validate::Validate` from
/// `#[validate(...)]` field attributes, so `Ctx::validated::<T>()` can parse and
/// validate a request body in one step.
///
/// Supported rules: `required`, `str`, `integer`, `number`, `bool`, `email`,
/// `url`, `alpha`, `alphanum`, `min_len = N`, `max_len = N`, `min = N`,
/// `max = N`, `same = "field"`. Validation keys are the struct field names.
#[proc_macro_derive(Validate, attributes(validate))]
pub fn derive_validate(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand_validate(input) {
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
    // Default the table name to the snake_case, pluralized struct name.
    let table = table.unwrap_or_else(|| pluralize(&to_snake(&name.to_string())));

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
        let mut skip = false;
        let mut relation: Option<RelationInfo> = None;
        let mut is_vector = false;
        let mut vector_dim: Option<usize> = None;

        for attr in &field.attrs {
            if attr.path().is_ident("model") {
                attr.parse_nested_meta(|meta| {
                    if meta.path.is_ident("primary") {
                        primary = true;
                        Ok(())
                    } else if meta.path.is_ident("skip") {
                        skip = true;
                        Ok(())
                    } else if meta.path.is_ident("vector") {
                        // #[model(vector)] or #[model(vector(dim = N))] on a Vec<f32>.
                        is_vector = true;
                        if meta.input.peek(syn::token::Paren) {
                            meta.parse_nested_meta(|nested| {
                                if nested.path.is_ident("dim") {
                                    let lit: syn::LitInt = nested.value()?.parse()?;
                                    vector_dim = Some(lit.base10_parse::<usize>()?);
                                    Ok(())
                                } else {
                                    Err(nested.error("expected `dim = N` in #[model(vector(...))]"))
                                }
                            })?;
                        }
                        Ok(())
                    } else if meta.path.is_ident("column") {
                        let lit: syn::LitStr = meta.value()?.parse()?;
                        column = lit.value();
                        Ok(())
                    } else if let Some(kind) = rel_kind(&meta.path) {
                        // has_many(Post, foreign_key = "user_id"[, local_key = "..."])
                        // belongs_to(Team, foreign_key = "team_id"[, owner_key = "..."])
                        let mut related: Option<syn::Path> = None;
                        let mut fk: Option<String> = None;
                        let mut key: Option<String> = None;
                        meta.parse_nested_meta(|nested| {
                            if nested.path.is_ident("foreign_key") {
                                fk = Some(nested.value()?.parse::<syn::LitStr>()?.value());
                                Ok(())
                            } else if nested.path.is_ident("local_key")
                                || nested.path.is_ident("owner_key")
                            {
                                key = Some(nested.value()?.parse::<syn::LitStr>()?.value());
                                Ok(())
                            } else if related.is_none() {
                                // The first bare item is the related model type.
                                related = Some(nested.path.clone());
                                Ok(())
                            } else {
                                Err(nested.error("unexpected argument in relation attribute"))
                            }
                        })?;
                        let related = related.ok_or_else(|| {
                            meta.error("relation needs a related model type, e.g. has_many(Post, foreign_key = \"user_id\")")
                        })?;
                        let foreign_key = fk.ok_or_else(|| {
                            meta.error("relation needs foreign_key = \"<column>\"")
                        })?;
                        relation = Some(RelationInfo { kind, related, foreign_key, key });
                        Ok(())
                    } else {
                        Err(meta.error("unknown #[model(...)] key on field (expected `primary`, `skip`, `column`, `vector`, `has_many`, `has_one`, or `belongs_to`)"))
                    }
                })?;
            }
        }

        // Relation fields and skipped fields are not columns; their Rust type is
        // irrelevant to the schema, so don't run `classify` on them.
        if skip || relation.is_some() {
            infos.push(FieldInfo {
                ident,
                column,
                scalar: Scalar::Text,
                optional: false,
                primary: false,
                skip,
                relation,
                vector_dim: None,
            });
            continue;
        }

        // A `#[model(vector)]` field is a `Vec<f32>` (optionally `Option<…>`),
        // which `classify` doesn't handle — resolve it directly to a vector column.
        if is_vector {
            let optional = last_segment(&field.ty).as_deref() == Some("Option");
            infos.push(FieldInfo {
                ident,
                column,
                scalar: Scalar::Vector,
                optional,
                primary: false,
                skip: false,
                relation: None,
                vector_dim,
            });
            continue;
        }

        let (scalar, optional) = classify(&field.ty).ok_or_else(|| {
            syn::Error::new_spanned(
                &field.ty,
                "unsupported field type for #[derive(Model)] (use i64/i32, f64/f32, String, bool, Json, or Option<…> of those; mark relations/vectors/ignored fields with #[model(...)])",
            )
        })?;

        infos.push(FieldInfo {
            ident,
            column,
            scalar,
            optional,
            primary,
            skip: false,
            relation: None,
            vector_dim: None,
        });
    }

    // ---- schema() columns ---- (skipped fields and relations are not columns)
    let column_defs = infos.iter().filter(|f| f.is_column()).map(|f| {
        let col = &f.column;
        let ty = match f.scalar {
            Scalar::Int => quote!(::sutegi::orm::ColType::Integer),
            Scalar::Real => quote!(::sutegi::orm::ColType::Real),
            Scalar::Text => quote!(::sutegi::orm::ColType::Text),
            Scalar::Bool => quote!(::sutegi::orm::ColType::Boolean),
            Scalar::Json => quote!(::sutegi::orm::ColType::Json),
            Scalar::Vector => {
                let dim = match f.vector_dim {
                    Some(d) => quote!(::core::option::Option::Some(#d)),
                    None => quote!(::core::option::Option::None),
                };
                quote!(::sutegi::orm::ColType::Vector { dim: #dim })
            }
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
        if !f.is_column() {
            // Skipped fields and (unloaded) relations start at their default;
            // relations are populated later by the generated `with_*` loaders.
            return quote! { #ident: ::core::default::Default::default() };
        }
        let extractor = match (f.scalar, f.optional) {
            (Scalar::Int, false) => quote!(::sutegi::orm::row::get_i64),
            (Scalar::Real, false) => quote!(::sutegi::orm::row::get_f64),
            (Scalar::Text, false) => quote!(::sutegi::orm::row::get_string),
            (Scalar::Bool, false) => quote!(::sutegi::orm::row::get_bool),
            (Scalar::Json, false) => quote!(::sutegi::orm::row::get_json),
            (Scalar::Vector, false) => quote!(::sutegi::orm::row::get_vector),
            (Scalar::Int, true) => quote!(::sutegi::orm::row::opt_i64),
            (Scalar::Real, true) => quote!(::sutegi::orm::row::opt_f64),
            (Scalar::Text, true) => quote!(::sutegi::orm::row::opt_string),
            (Scalar::Bool, true) => quote!(::sutegi::orm::row::opt_bool),
            (Scalar::Json, true) => quote!(::sutegi::orm::row::opt_json),
            (Scalar::Vector, true) => quote!(::sutegi::orm::row::opt_vector),
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

    // ---- from_input() field initializers ---- (lenient: absent non-nullable
    // columns become the type default instead of erroring, so a partial client
    // payload — no `id`, no `done` — still hydrates).
    let from_input_inits = infos.iter().map(|f| {
        let ident = &f.ident;
        let col = &f.column;
        if !f.is_column() {
            return quote! { #ident: ::core::default::Default::default() };
        }
        let opt = match f.scalar {
            Scalar::Int => quote!(::sutegi::orm::row::opt_i64),
            Scalar::Real => quote!(::sutegi::orm::row::opt_f64),
            Scalar::Text => quote!(::sutegi::orm::row::opt_string),
            Scalar::Bool => quote!(::sutegi::orm::row::opt_bool),
            Scalar::Json => quote!(::sutegi::orm::row::opt_json),
            Scalar::Vector => quote!(::sutegi::orm::row::opt_vector),
        };
        let cast = matches!(f.scalar, Scalar::Int | Scalar::Real);
        if f.optional {
            if cast {
                quote! { #ident: #opt(row, #col)?.map(|v| v as _) }
            } else {
                quote! { #ident: #opt(row, #col)? }
            }
        } else {
            // Non-nullable: default the value when the client omits it.
            let default = match f.scalar {
                Scalar::Json => quote!(.unwrap_or(::sutegi::json::Json::Null)),
                _ => quote!(.unwrap_or_default()),
            };
            if cast {
                quote! { #ident: #opt(row, #col)?#default as _ }
            } else {
                quote! { #ident: #opt(row, #col)?#default }
            }
        }
    });

    // ---- to_values() pairs ---- (skipped fields and relations are not persisted)
    let to_values = infos.iter().filter(|f| f.is_column()).map(|f| {
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

    // ---- save() pairs ---- (like to_values, but drop the primary key so the
    // backend assigns it; `save` returns the new id).
    let save_values = infos
        .iter()
        .filter(|f| f.is_column() && !f.primary)
        .map(|f| {
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

    // ---- to_json() pairs ---- (skipped fields are not serialized; relations
    // are nested under their field name so a loaded parent serializes with its
    // children — introspectable straight out of a handler).
    let to_json = infos.iter().filter(|f| !f.skip).map(|f| {
        let ident = &f.ident;
        let col = &f.column;
        if let Some(rel) = &f.relation {
            return match rel.kind {
                RelKind::HasMany => quote! {
                    (#col, ::sutegi::json::Json::arr(
                        self.#ident.iter().map(|r| r.to_json()).collect()
                    ))
                },
                RelKind::HasOne | RelKind::BelongsTo => quote! {
                    (#col, match &self.#ident {
                        ::core::option::Option::Some(r) => r.to_json(),
                        ::core::option::Option::None => ::sutegi::json::Json::Null,
                    })
                },
            };
        }
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

    let relation_loaders = relation_loaders(&infos)?;

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

        impl ::sutegi::orm::FromInput for #name {
            fn from_input(row: &::sutegi::json::Json) -> ::core::result::Result<Self, ::std::string::String> {
                ::core::result::Result::Ok(Self { #( #from_input_inits ),* })
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

            /// Insert this record over any [`Backend`](::sutegi::orm::Backend),
            /// letting the database assign the primary key, and return the new
            /// id. The primary-key field on `self` is ignored.
            pub fn save<B: ::sutegi::orm::Backend>(
                &self,
                conn: &B,
            ) -> ::core::result::Result<i64, ::std::string::String> {
                conn.insert(
                    <Self as ::sutegi::orm::Model>::table(),
                    &[ #( #save_values ),* ],
                    <Self as ::sutegi::orm::Model>::primary_key(),
                )
            }

            #( #relation_loaders )*
        }
    })
}

/// Generate a batch eager-loader (`with_<field>`) for each relation field.
///
/// Each loader takes the parents and runs a single `WHERE key IN (…)` query for
/// the whole batch — the classic two-query strategy that sidesteps N+1 — then
/// hydrates and attaches the typed children. Join keys are integers (the usual
/// primary/foreign-key case).
fn relation_loaders(infos: &[FieldInfo]) -> syn::Result<Vec<proc_macro2::TokenStream>> {
    // A column field's ident by its column name (for resolving join keys).
    let ident_for_column = |col: &str| {
        infos
            .iter()
            .find(|f| f.is_column() && f.column == col)
            .map(|f| &f.ident)
    };
    let primary_column = infos
        .iter()
        .find(|f| f.primary)
        .map(|f| f.column.clone())
        .unwrap_or_else(|| "id".to_string());

    let mut loaders = Vec::new();
    for f in infos {
        let Some(rel) = &f.relation else { continue };
        let ident = &f.ident;
        let method = syn::Ident::new(&format!("with_{ident}"), ident.span());
        let related = &rel.related;
        let fk = &rel.foreign_key;

        // The parent field whose (integer) value joins to the children, and the
        // child column that value is matched against.
        let (parent_key_ident, child_col): (&syn::Ident, proc_macro2::TokenStream) = match rel.kind
        {
            RelKind::HasMany | RelKind::HasOne => {
                let local_col = rel.key.clone().unwrap_or_else(|| primary_column.clone());
                let id = ident_for_column(&local_col).ok_or_else(|| {
                    syn::Error::new_spanned(
                        ident,
                        format!("relation local key column `{local_col}` has no matching field on this model"),
                    )
                })?;
                (id, quote! { #fk })
            }
            RelKind::BelongsTo => {
                let id = ident_for_column(fk).ok_or_else(|| {
                    syn::Error::new_spanned(
                        ident,
                        format!(
                            "belongs_to foreign_key `{fk}` must name a column field on this model"
                        ),
                    )
                })?;
                let owner_col = match &rel.key {
                    Some(k) => quote! { #k },
                    None => quote! { <#related as ::sutegi::orm::Model>::primary_key() },
                };
                (id, owner_col)
            }
        };

        let attach = if rel.kind == RelKind::HasMany {
            // Group children by key, hand each parent its (possibly empty) Vec.
            quote! {
                let mut groups: ::std::collections::HashMap<i64, ::std::vec::Vec<#related>> =
                    ::std::collections::HashMap::new();
                for row in &rows {
                    let k = row.get(child_col).and_then(::sutegi::json::Json::as_i64)
                        .ok_or_else(|| ::std::format!("relation key `{}` missing or non-integer", child_col))?;
                    groups.entry(k).or_default().push(
                        <#related as ::sutegi::orm::row::FromRow>::from_row(row)?
                    );
                }
                for p in &mut parents {
                    let key = p.#parent_key_ident as i64;
                    p.#ident = groups.remove(&key).unwrap_or_default();
                }
            }
        } else {
            // has_one / belongs_to: first child per key, hydrated on attach.
            quote! {
                let mut by_key: ::std::collections::HashMap<i64, &::sutegi::json::Json> =
                    ::std::collections::HashMap::new();
                for row in &rows {
                    if let ::core::option::Option::Some(k) =
                        row.get(child_col).and_then(::sutegi::json::Json::as_i64)
                    {
                        by_key.entry(k).or_insert(row);
                    }
                }
                for p in &mut parents {
                    let key = p.#parent_key_ident as i64;
                    p.#ident = match by_key.get(&key) {
                        ::core::option::Option::Some(row) =>
                            ::core::option::Option::Some(
                                <#related as ::sutegi::orm::row::FromRow>::from_row(row)?
                            ),
                        ::core::option::Option::None => ::core::option::Option::None,
                    };
                }
            }
        };

        let doc = format!(
            "Eager-load the `{ident}` relation for a batch of rows in a single query \
             (no N+1), returning the parents with the relation populated."
        );
        loaders.push(quote! {
            #[doc = #doc]
            pub fn #method<B: ::sutegi::orm::Backend>(
                conn: &B,
                mut parents: ::std::vec::Vec<Self>,
            ) -> ::core::result::Result<::std::vec::Vec<Self>, ::std::string::String> {
                if parents.is_empty() {
                    return ::core::result::Result::Ok(parents);
                }
                let child_col: &str = #child_col;
                let keys: ::std::vec::Vec<::sutegi::orm::Value> = parents
                    .iter()
                    .map(|p| ::sutegi::orm::Value::Int(p.#parent_key_ident as i64))
                    .collect();
                let rows = ::sutegi::orm::Backend::select(
                    conn,
                    &<#related as ::sutegi::orm::Model>::query().filter_in(child_col, keys),
                )?;
                #attach
                ::core::result::Result::Ok(parents)
            }
        });
    }
    Ok(loaders)
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
        "Json" => Some(Scalar::Json),
        _ => None,
    }
}

fn value_wrapper(scalar: Scalar) -> proc_macro2::TokenStream {
    match scalar {
        Scalar::Int => quote!(|v: _| ::sutegi::orm::Value::Int(v as i64)),
        Scalar::Real => quote!(|v: _| ::sutegi::orm::Value::Real(v as f64)),
        Scalar::Text => quote!(|v: ::std::string::String| ::sutegi::orm::Value::Text(v)),
        Scalar::Bool => quote!(|v: bool| ::sutegi::orm::Value::Bool(v)),
        Scalar::Json => quote!(|v: ::sutegi::json::Json| ::sutegi::orm::Value::Json(v)),
        Scalar::Vector => {
            quote!(|v: ::std::vec::Vec<f32>| ::sutegi::orm::Value::Vector(v))
        }
    }
}

fn json_wrapper(scalar: Scalar) -> proc_macro2::TokenStream {
    match scalar {
        Scalar::Int => quote!(|v: _| ::sutegi::json::Json::int(v as i64)),
        Scalar::Real => quote!(|v: _| ::sutegi::json::Json::Num(v as f64)),
        Scalar::Text => quote!(|v: ::std::string::String| ::sutegi::json::Json::Str(v)),
        Scalar::Bool => quote!(|v: bool| ::sutegi::json::Json::Bool(v)),
        // A JSON field serializes as itself.
        Scalar::Json => quote!(|v: ::sutegi::json::Json| v),
        Scalar::Vector => quote!(|v: ::std::vec::Vec<f32>| ::sutegi::json::Json::arr(
            v.iter()
                .map(|x| ::sutegi::json::Json::Num(*x as f64))
                .collect()
        )),
    }
}

fn last_segment(ty: &Type) -> Option<String> {
    if let Type::Path(p) = ty {
        p.path.segments.last().map(|s| s.ident.to_string())
    } else {
        None
    }
}

/// `CamelCase` → `camel_case` (for default table names).
fn to_snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Naive English pluralization, matching the CLI's convention.
fn pluralize(word: &str) -> String {
    if word.ends_with('s') {
        word.to_string()
    } else if let Some(stem) = word.strip_suffix('y') {
        format!("{}ies", stem)
    } else {
        format!("{}s", word)
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

/// Parse a numeric literal (`5` or `5.0`) as an `f64` for `min`/`max` rules.
fn lit_f64(lit: &syn::Lit) -> syn::Result<f64> {
    match lit {
        syn::Lit::Int(i) => i.base10_parse::<f64>(),
        syn::Lit::Float(f) => f.base10_parse::<f64>(),
        other => Err(syn::Error::new_spanned(other, "expected a number")),
    }
}

fn expand_validate(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let name = &input.ident;
    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return Err(syn::Error::new_spanned(
                    name,
                    "#[derive(Validate)] requires named fields",
                ))
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                name,
                "#[derive(Validate)] can only be derived for structs",
            ))
        }
    };

    let mut field_rules: Vec<proc_macro2::TokenStream> = Vec::new();
    for f in fields {
        let ident = f.ident.as_ref().unwrap();
        let mut rules: Vec<proc_macro2::TokenStream> = Vec::new();
        for attr in &f.attrs {
            if !attr.path().is_ident("validate") {
                continue;
            }
            attr.parse_nested_meta(|meta| {
                let p = &meta.path;
                let simple = |name: &str| -> Option<proc_macro2::TokenStream> {
                    if p.is_ident(name) {
                        let variant = syn::Ident::new(
                            match name {
                                "str" => "Str",
                                "integer" => "Integer",
                                "number" => "Number",
                                "bool" => "Bool",
                                "required" => "Required",
                                "email" => "Email",
                                "url" => "Url",
                                "alpha" => "Alpha",
                                "alphanum" => "AlphaNum",
                                _ => return None,
                            },
                            proc_macro2::Span::call_site(),
                        );
                        Some(quote! { ::sutegi::validate::Rule::#variant })
                    } else {
                        None
                    }
                };
                for name in [
                    "required", "str", "integer", "number", "bool", "email", "url", "alpha",
                    "alphanum",
                ] {
                    if let Some(tok) = simple(name) {
                        rules.push(tok);
                        return Ok(());
                    }
                }
                if p.is_ident("min_len") {
                    let v: syn::LitInt = meta.value()?.parse()?;
                    let n: usize = v.base10_parse()?;
                    rules.push(quote! { ::sutegi::validate::Rule::MinLen(#n) });
                } else if p.is_ident("max_len") {
                    let v: syn::LitInt = meta.value()?.parse()?;
                    let n: usize = v.base10_parse()?;
                    rules.push(quote! { ::sutegi::validate::Rule::MaxLen(#n) });
                } else if p.is_ident("min") {
                    let lit: syn::Lit = meta.value()?.parse()?;
                    let n = lit_f64(&lit)?;
                    rules.push(quote! { ::sutegi::validate::Rule::Min(#n) });
                } else if p.is_ident("max") {
                    let lit: syn::Lit = meta.value()?.parse()?;
                    let n = lit_f64(&lit)?;
                    rules.push(quote! { ::sutegi::validate::Rule::Max(#n) });
                } else if p.is_ident("same") {
                    let v: syn::LitStr = meta.value()?.parse()?;
                    let other = v.value();
                    rules.push(quote! { ::sutegi::validate::Rule::Same(#other.to_string()) });
                } else {
                    return Err(meta.error("unknown #[validate(...)] rule"));
                }
                Ok(())
            })?;
        }
        if !rules.is_empty() {
            let key = ident.to_string();
            field_rules.push(quote! { .field(#key, &[ #( #rules ),* ]) });
        }
    }

    Ok(quote! {
        impl ::sutegi::validate::Validate for #name {
            fn rules() -> ::sutegi::validate::Ruleset {
                ::sutegi::validate::Ruleset::new() #( #field_rules )*
            }
        }
    })
}
