use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::{Attribute, Data, DeriveInput, Fields, GenericParam, Ident, LitStr};

struct Container {
    rename: Option<String>,
    tag: Option<String>,
    krate: syn::Path,
    doc: String,
}

struct FieldSpec {
    member: syn::Member,
    binding: Ident,
    ty: syn::Type,
    wire: String,
    doc: String,
    default: bool,
    skip: bool,
    rules: Rules,
}

#[derive(Default)]
struct Rules {
    min_len: Option<syn::Expr>,
    max_len: Option<syn::Expr>,
    min: Option<syn::Expr>,
    max: Option<syn::Expr>,
}

struct VariantSpec {
    ident: Ident,
    wire: String,
    doc: String,
    kind: VariantKind,
}

enum VariantKind {
    Unit,
    Named(Vec<FieldSpec>),
    Newtype(syn::Type),
}

pub(crate) fn expand(input: DeriveInput) -> syn::Result<TokenStream> {
    let container = container(&input)?;
    for p in &input.generics.params {
        if !matches!(p, GenericParam::Type(_)) {
            return Err(syn::Error::new_spanned(
                p,
                "#[derive(Contract)] supports type parameters only (no lifetimes or const generics)",
            ));
        }
    }
    let c = &container.krate;
    let ident = &input.ident;
    let base = container
        .rename
        .clone()
        .unwrap_or_else(|| ident.to_string());
    let doc = &container.doc;

    let mut generics = input.generics.clone();
    let type_params: Vec<Ident> = generics.type_params().map(|t| t.ident.clone()).collect();
    {
        let wc = generics.make_where_clause();
        for t in &type_params {
            wc.predicates.push(syn::parse_quote!(#t: #c::Contract));
        }
    }
    let (impl_g, ty_g, where_c) = generics.split_for_impl();

    let (shape, decode, encode, missing) = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => {
                if container.tag.is_some() {
                    return Err(syn::Error::new_spanned(
                        ident,
                        "#[contract(tag = ...)] applies to enums only",
                    ));
                }
                let fields = named
                    .named
                    .iter()
                    .enumerate()
                    .map(|(i, f)| field_spec(f, i))
                    .collect::<syn::Result<Vec<_>>>()?;
                struct_parts(c, &fields)?
            }
            Fields::Unnamed(un) if un.unnamed.len() == 1 => {
                let f = field_spec(&un.unnamed[0], 0)?;
                newtype_parts(c, &f)?
            }
            _ => {
                return Err(syn::Error::new_spanned(
                    ident,
                    "#[derive(Contract)] needs named fields or a single-field newtype",
                ))
            }
        },
        Data::Enum(e) => {
            let variants = e
                .variants
                .iter()
                .map(variant_spec)
                .collect::<syn::Result<Vec<_>>>()?;
            if variants.is_empty() {
                return Err(syn::Error::new_spanned(
                    ident,
                    "an empty enum has no wire form",
                ));
            }
            let all_unit = variants.iter().all(|v| matches!(v.kind, VariantKind::Unit));
            match &container.tag {
                Some(tag) => union_parts(c, tag, &variants)?,
                None if all_unit => enum_parts(c, &variants)?,
                None => {
                    return Err(syn::Error::new_spanned(
                        ident,
                        "an enum with data needs #[contract(tag = \"...\")] naming its discriminant field",
                    ))
                }
            }
        }
        Data::Union(_) => {
            return Err(syn::Error::new_spanned(
                ident,
                "#[derive(Contract)] does not support unions",
            ))
        }
    };

    Ok(quote! {
        impl #impl_g #c::Contract for #ident #ty_g #where_c {
            fn describe(__reg: &mut #c::Registry) -> #c::Type {
                let mut __name = ::std::string::String::from(#base);
                #(
                    __name.push('_');
                    __name.push_str(&<#type_params as #c::Contract>::describe(__reg).label());
                )*
                __reg.define(&__name, ::std::any::type_name::<Self>(), #doc, |__reg| #shape)
            }

            fn decode(__value: &#c::Json, __cx: &mut #c::Cx) -> ::std::option::Option<Self> {
                #decode
            }

            fn encode(&self) -> #c::Json {
                #encode
            }

            #missing
        }
    })
}

fn container(input: &DeriveInput) -> syn::Result<Container> {
    let mut out = Container {
        rename: None,
        tag: None,
        krate: syn::parse_quote!(::sutegi::contract),
        doc: doc_of(&input.attrs),
    };
    for attr in &input.attrs {
        if !attr.path().is_ident("contract") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                out.rename = Some(ident_string(&meta.value()?.parse::<LitStr>()?)?);
            } else if meta.path.is_ident("tag") {
                out.tag = Some(meta.value()?.parse::<LitStr>()?.value());
            } else if meta.path.is_ident("crate") {
                out.krate = meta.value()?.parse::<LitStr>()?.parse()?;
            } else if meta.path.is_ident("doc") {
                out.doc = meta.value()?.parse::<LitStr>()?.value();
            } else {
                return Err(
                    meta.error("unknown #[contract(...)] key (expected rename, tag, crate, doc)")
                );
            }
            Ok(())
        })?;
    }
    Ok(out)
}

fn ident_string(lit: &LitStr) -> syn::Result<String> {
    let v = lit.value();
    let ok = v
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(v)
    } else {
        Err(syn::Error::new_spanned(
            lit,
            "a contract type name must be an identifier",
        ))
    }
}

fn doc_of(attrs: &[Attribute]) -> String {
    let mut lines = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let syn::Meta::NameValue(nv) = &attr.meta {
            if let syn::Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = &nv.value
            {
                let v = s.value();
                lines.push(v.strip_prefix(' ').unwrap_or(&v).trim_end().to_string());
            }
        }
    }
    lines.join("\n").trim().to_string()
}

fn field_spec(f: &syn::Field, index: usize) -> syn::Result<FieldSpec> {
    let member = match &f.ident {
        Some(i) => syn::Member::Named(i.clone()),
        None => syn::Member::Unnamed(syn::Index::from(index)),
    };
    let mut spec = FieldSpec {
        wire: f.ident.as_ref().map(unraw).unwrap_or_default(),
        member,
        binding: format_ident!("__f{}", index),
        ty: f.ty.clone(),
        doc: doc_of(&f.attrs),
        default: false,
        skip: false,
        rules: Rules::default(),
    };
    for attr in &f.attrs {
        if !attr.path().is_ident("contract") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            let p = &meta.path;
            if p.is_ident("rename") {
                spec.wire = meta.value()?.parse::<LitStr>()?.value();
            } else if p.is_ident("doc") {
                spec.doc = meta.value()?.parse::<LitStr>()?.value();
            } else if p.is_ident("default") {
                spec.default = true;
            } else if p.is_ident("skip") {
                spec.skip = true;
            } else if p.is_ident("min_len") {
                spec.rules.min_len = Some(meta.value()?.parse()?);
            } else if p.is_ident("max_len") {
                spec.rules.max_len = Some(meta.value()?.parse()?);
            } else if p.is_ident("min") {
                spec.rules.min = Some(meta.value()?.parse()?);
            } else if p.is_ident("max") {
                spec.rules.max = Some(meta.value()?.parse()?);
            } else {
                return Err(meta.error(
                    "unknown #[contract(...)] field key (expected rename, doc, default, skip, min_len, max_len, min, max)",
                ));
            }
            Ok(())
        })?;
    }
    Ok(spec)
}

fn unraw(i: &Ident) -> String {
    let s = i.to_string();
    s.strip_prefix("r#").map(str::to_string).unwrap_or(s)
}

fn snake(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn variant_spec(v: &syn::Variant) -> syn::Result<VariantSpec> {
    let mut wire = snake(&unraw(&v.ident));
    let mut doc = doc_of(&v.attrs);
    for attr in &v.attrs {
        if !attr.path().is_ident("contract") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                wire = meta.value()?.parse::<LitStr>()?.value();
            } else if meta.path.is_ident("doc") {
                doc = meta.value()?.parse::<LitStr>()?.value();
            } else {
                return Err(
                    meta.error("unknown #[contract(...)] variant key (expected rename, doc)")
                );
            }
            Ok(())
        })?;
    }
    let kind = match &v.fields {
        Fields::Unit => VariantKind::Unit,
        Fields::Named(named) => VariantKind::Named(
            named
                .named
                .iter()
                .enumerate()
                .map(|(i, f)| field_spec(f, i))
                .collect::<syn::Result<_>>()?,
        ),
        Fields::Unnamed(un) if un.unnamed.len() == 1 => VariantKind::Newtype(un.unnamed[0].ty.clone()),
        Fields::Unnamed(_) => {
            return Err(syn::Error::new_spanned(
                v,
                "a tuple variant must have exactly one field (a struct whose fields are merged next to the tag)",
            ))
        }
    };
    Ok(VariantSpec {
        ident: v.ident.clone(),
        wire,
        doc,
        kind,
    })
}

fn opt_tokens(e: &Option<syn::Expr>, ty: TokenStream) -> TokenStream {
    match e {
        Some(e) => quote!(::std::option::Option::Some((#e) as #ty)),
        None => quote!(::std::option::Option::None),
    }
}

fn rules_tokens(c: &syn::Path, r: &Rules) -> TokenStream {
    let min_len = opt_tokens(&r.min_len, quote!(u64));
    let max_len = opt_tokens(&r.max_len, quote!(u64));
    let min = opt_tokens(&r.min, quote!(f64));
    let max = opt_tokens(&r.max, quote!(f64));
    quote!(#c::Rules { min_len: #min_len, max_len: #max_len, min: #min, max: #max })
}

fn check_unique(fields: &[FieldSpec], reserved: Option<&str>) -> syn::Result<()> {
    let mut seen: Vec<&str> = reserved.into_iter().collect();
    for f in fields.iter().filter(|f| !f.skip) {
        if seen.contains(&f.wire.as_str()) {
            return Err(syn::Error::new_spanned(
                &f.ty,
                format!(
                    "wire name {:?} is used twice (or collides with the tag)",
                    f.wire
                ),
            ));
        }
        seen.push(&f.wire);
    }
    Ok(())
}

fn describe_fields(c: &syn::Path, fields: &[FieldSpec]) -> TokenStream {
    let items = fields.iter().filter(|f| !f.skip).map(|f| {
        let (wire, doc, ty) = (&f.wire, &f.doc, &f.ty);
        let rules = rules_tokens(c, &f.rules);
        let required = if f.default {
            quote!(false)
        } else {
            quote!(<#ty as #c::Contract>::missing().is_none())
        };
        quote! {
            #c::Field {
                name: ::std::string::String::from(#wire),
                doc: ::std::string::String::from(#doc),
                ty: <#ty as #c::Contract>::describe(__reg),
                required: #required,
                rules: #rules,
            }
        }
    });
    quote!(::std::vec![#(#items),*])
}

fn decode_lets(c: &syn::Path, fields: &[FieldSpec]) -> TokenStream {
    let lets = fields.iter().map(|f| {
        let (wire, ty, b) = (&f.wire, &f.ty, &f.binding);
        if f.skip {
            return quote!(let #b: ::std::option::Option<#ty> = ::std::option::Option::Some(::std::default::Default::default()););
        }
        let rules = rules_tokens(c, &f.rules);
        if f.default {
            quote!(let #b = __cx.field_or::<#ty>(__obj, #wire, &#rules, ::std::default::Default::default);)
        } else {
            quote!(let #b = __cx.field::<#ty>(__obj, #wire, &#rules);)
        }
    });
    quote!(#(#lets)*)
}

fn construct(fields: &[FieldSpec]) -> TokenStream {
    let parts = fields.iter().map(|f| {
        let (m, b) = (&f.member, &f.binding);
        quote!(#m: #b?)
    });
    quote!({ #(#parts),* })
}

fn encode_inserts(
    c: &syn::Path,
    fields: &[FieldSpec],
    access: impl Fn(&FieldSpec) -> TokenStream,
) -> TokenStream {
    let inserts = fields.iter().filter(|f| !f.skip).map(|f| {
        let wire = &f.wire;
        let a = access(f);
        quote!(__m.insert(::std::string::String::from(#wire), #c::Contract::encode(#a));)
    });
    quote!(#(#inserts)*)
}

type Parts = (TokenStream, TokenStream, TokenStream, TokenStream);

fn struct_parts(c: &syn::Path, fields: &[FieldSpec]) -> syn::Result<Parts> {
    check_unique(fields, None)?;
    let describe = describe_fields(c, fields);
    let lets = decode_lets(c, fields);
    let build = construct(fields);
    let inserts = encode_inserts(c, fields, |f| {
        let m = &f.member;
        quote!(&self.#m)
    });
    Ok((
        quote!(#c::Shape::Struct(#describe)),
        quote! {
            if let ::std::option::Option::Some(__parsed) = __cx.reparse(__value) {
                return <Self as #c::Contract>::decode(&__parsed, __cx);
            }
            let __obj = __cx.object(__value)?;
            #lets
            ::std::option::Option::Some(Self #build)
        },
        quote! {
            let mut __m = ::std::collections::BTreeMap::new();
            #inserts
            #c::Json::Obj(__m)
        },
        TokenStream::new(),
    ))
}

fn newtype_parts(c: &syn::Path, f: &FieldSpec) -> syn::Result<Parts> {
    if f.skip || f.default {
        return Err(syn::Error::new_spanned(
            &f.ty,
            "a newtype's only field cannot be skipped or defaulted",
        ));
    }
    let ty = &f.ty;
    let rules = rules_tokens(c, &f.rules);
    Ok((
        quote!(#c::Shape::Alias(<#ty as #c::Contract>::describe(__reg))),
        quote!(__cx.checked::<#ty>(__value, &#rules).map(Self)),
        quote!(#c::Contract::encode(&self.0)),
        quote! {
            fn missing() -> ::std::option::Option<Self> {
                <#ty as #c::Contract>::missing().map(Self)
            }
        },
    ))
}

fn expected_list(variants: &[VariantSpec]) -> String {
    let names: Vec<String> = variants.iter().map(|v| format!("{:?}", v.wire)).collect();
    format!("one of: {}", names.join(", "))
}

fn check_variants(variants: &[VariantSpec]) -> syn::Result<()> {
    for (i, v) in variants.iter().enumerate() {
        if variants[..i].iter().any(|w| w.wire == v.wire) {
            return Err(syn::Error::new_spanned(
                &v.ident,
                format!("variant wire name {:?} is used twice", v.wire),
            ));
        }
    }
    Ok(())
}

fn enum_parts(c: &syn::Path, variants: &[VariantSpec]) -> syn::Result<Parts> {
    check_variants(variants)?;
    let describe = variants.iter().map(|v| {
        let (wire, doc) = (&v.wire, &v.doc);
        quote!(#c::Variant { name: ::std::string::String::from(#wire), doc: ::std::string::String::from(#doc), shape: #c::VariantShape::Unit })
    });
    let decode_arms = variants.iter().map(|v| {
        let (wire, id) = (&v.wire, &v.ident);
        quote!(#wire => ::std::option::Option::Some(Self::#id),)
    });
    let encode_arms = variants.iter().map(|v| {
        let (wire, id) = (&v.wire, &v.ident);
        quote!(Self::#id => #wire,)
    });
    let expected = expected_list(variants);
    Ok((
        quote!(#c::Shape::Enum(::std::vec![#(#describe),*])),
        quote! {
            match __value {
                #c::Json::Str(__s) => match __s.as_str() {
                    #(#decode_arms)*
                    _ => __cx.expected(#expected),
                },
                _ => __cx.expected(#expected),
            }
        },
        quote!(#c::Json::Str(::std::string::String::from(match self { #(#encode_arms)* }))),
        TokenStream::new(),
    ))
}

fn union_parts(c: &syn::Path, tag: &str, variants: &[VariantSpec]) -> syn::Result<Parts> {
    check_variants(variants)?;
    let mut describe = Vec::new();
    let mut decode_arms = Vec::new();
    let mut encode_arms = Vec::new();
    for v in variants {
        let (wire, doc, id) = (&v.wire, &v.doc, &v.ident);
        let tag_insert = quote!(__m.insert(::std::string::String::from(#tag), #c::Json::Str(::std::string::String::from(#wire)));
        );
        match &v.kind {
            VariantKind::Unit => {
                describe.push(quote!(#c::Variant { name: ::std::string::String::from(#wire), doc: ::std::string::String::from(#doc), shape: #c::VariantShape::Unit }));
                decode_arms.push(quote!(#wire => ::std::option::Option::Some(Self::#id),));
                encode_arms.push(quote!(Self::#id => {
                    let mut __m = ::std::collections::BTreeMap::new();
                    #tag_insert
                    #c::Json::Obj(__m)
                }));
            }
            VariantKind::Named(fields) => {
                check_unique(fields, Some(tag))?;
                let df = describe_fields(c, fields);
                describe.push(quote!(#c::Variant { name: ::std::string::String::from(#wire), doc: ::std::string::String::from(#doc), shape: #c::VariantShape::Struct(#df) }));
                let lets = decode_lets(c, fields);
                let build = construct(fields);
                decode_arms
                    .push(quote!(#wire => { #lets ::std::option::Option::Some(Self::#id #build) }));
                let pats = fields.iter().map(|f| {
                    let (m, b) = (&f.member, &f.binding);
                    quote!(#m: #b)
                });
                let inserts = encode_inserts(c, fields, |f| {
                    let b = &f.binding;
                    quote!(#b)
                });
                encode_arms.push(quote!(Self::#id { #(#pats),* } => {
                    let mut __m = ::std::collections::BTreeMap::new();
                    #tag_insert
                    #inserts
                    #c::Json::Obj(__m)
                }));
            }
            VariantKind::Newtype(ty) => {
                describe.push(quote!(#c::Variant { name: ::std::string::String::from(#wire), doc: ::std::string::String::from(#doc), shape: #c::VariantShape::Newtype(<#ty as #c::Contract>::describe(__reg)) }));
                decode_arms.push(
                    quote!(#wire => <#ty as #c::Contract>::decode(__value, __cx).map(Self::#id),),
                );
                encode_arms.push(quote!(Self::#id(__inner) => {
                    let mut __m = match #c::Contract::encode(__inner) {
                        #c::Json::Obj(__m) => __m,
                        __other => {
                            let mut __m = ::std::collections::BTreeMap::new();
                            __m.insert(::std::string::String::from("value"), __other);
                            __m
                        }
                    };
                    #tag_insert
                    #c::Json::Obj(__m)
                }));
            }
        }
    }
    let expected = expected_list(variants);
    Ok((
        quote!(#c::Shape::Union { tag: ::std::string::String::from(#tag), variants: ::std::vec![#(#describe),*] }),
        quote! {
            if let ::std::option::Option::Some(__parsed) = __cx.reparse(__value) {
                return <Self as #c::Contract>::decode(&__parsed, __cx);
            }
            let __obj = __cx.object(__value)?;
            let __tag = match __obj.get(#tag) {
                ::std::option::Option::Some(#c::Json::Str(__s)) => __s.as_str(),
                _ => return __cx.at_key(#tag, |__cx| __cx.expected(#expected)),
            };
            match __tag {
                #(#decode_arms)*
                _ => __cx.at_key(#tag, |__cx| __cx.expected(#expected)),
            }
        },
        quote!(match self { #(#encode_arms)* }),
        TokenStream::new(),
    ))
}
