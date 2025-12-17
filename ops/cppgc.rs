// Copyright 2018-2025 the Deno authors. MIT license.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{quote, quote_spanned};
use syn::parse::Parse;
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
  Attribute, Data, DeriveInput, Error, Fields, Ident, Meta, Result, Token,
  Type, parse_macro_input,
};

pub fn derives_inherits(input: TokenStream) -> TokenStream {
  match inherits_inner(parse_macro_input!(input as DeriveInput)) {
    Ok(tokens) => tokens.into(),
    Err(err) => err.to_compile_error().into(),
  }
}

pub fn derives_base(input: TokenStream) -> TokenStream {
  match base_inner(parse_macro_input!(input as DeriveInput)) {
    Ok(tokens) => tokens.into(),
    Err(err) => err.to_compile_error().into(),
  }
}

fn inherits_inner(input: DeriveInput) -> Result<TokenStream2> {
  let DeriveInput {
    ident,
    generics,
    data,
    attrs,
    ..
  } = input;

  let inheritance_list = parse_base_attr(&attrs)?;
  ensure_repr_c(&attrs, ident.span())?;

  let first_field = first_field(&data).ok_or_else(|| {
    Error::new(
      ident.span(),
      "cppgc inheritance requires at least one field",
    )
  })?;

  let (field_path, field_ty_span) = match &first_field.field {
    FieldRef::Named(ident, ty_span) => (quote!(#ident), *ty_span),
    FieldRef::Unnamed(idx, ty_span) => (quote!(#idx), *ty_span),
  };

  if !types_equal(&first_field.ty, &inheritance_list.ancestors[0]) {
    return Err(Error::new(
      field_ty_span,
      "first field must be the base type for cppgc inheritance",
    ));
  }

  let mut output = TokenStream2::new();
  for ancestor in inheritance_list.ancestors {
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let offset_assert = quote! {
      const _: () = {
        const OFFSET: usize = ::core::mem::offset_of!(#ident #ty_generics, #field_path);
        assert!(OFFSET == 0, "base field must be at offset 0");
      };
    };
    let size_align_assert = quote! {
      const _: () = {
        assert!(
          ::core::mem::size_of::<#ancestor>() != 0,
          "zero-sized base types are not supported for inheritance between cppgc types"
        );
        assert!(
          ::core::mem::align_of::<#ident #ty_generics>() >= ::core::mem::align_of::<#ancestor>(),
          "derived alignment must be >= base alignment for inheritance between cppgc types"
        );
      };
    };

    output.extend(quote! {
      #offset_assert
      #size_align_assert
      #[automatically_derived]
      unsafe impl #impl_generics deno_core::cppgc::Inherits<#ancestor> for #ident #ty_generics #where_clause {}
    });
  }
  Ok(output)
}

fn base_inner(input: DeriveInput) -> Result<TokenStream2> {
  let DeriveInput {
    ident,
    generics,
    data: _data,
    attrs,
    ..
  } = input;

  ensure_repr_c(&attrs, ident.span())?;

  let inheritors = parse_inheritors_attr(&attrs)?;
  if inheritors.is_empty() {
    return Err(Error::new(
      ident.span(),
      "cppgc base must list at least one inheriting type",
    ));
  }

  let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

  let type_ids = inheritors
    .iter()
    .map(|ty| quote!(::std::any::TypeId::of::<#ty>()));

  let check_name = quote::format_ident!("assert_inherits_{}", ident);
  let inherits_checks = inheritors.iter().map(|ty| {
    quote_spanned! {ty.span()=>
      const _: () = {
        #[allow(nonstandard_style)]
        fn #check_name<T: deno_core::cppgc::Inherits<#ident #ty_generics>>() {}
        let _ = #check_name::<#ty>;
      };
    }
  });

  let size_assert = quote! {
    const _: () = {
      assert!(
        ::core::mem::size_of::<#ident #ty_generics>() != 0,
        "zero-sized base types are not supported for cppgc inheritance"
      );
    };
  };

  Ok(quote! {
    #size_assert
    const _: () = { #( #inherits_checks )* };
    #[automatically_derived]
    unsafe impl #impl_generics deno_core::cppgc::Base for #ident #ty_generics #where_clause {
      const INHERITING_TYPES: &[::std::any::TypeId] = &[ #( #type_ids ),* ];
    }
  })
}

fn ensure_repr_c(attrs: &[Attribute], span: proc_macro2::Span) -> Result<()> {
  for attr in attrs {
    if !attr.path().is_ident("repr") {
      continue;
    }
    if let Meta::List(list) = &attr.meta {
      let nested: Punctuated<Meta, Token![,]> =
        list.parse_args_with(Punctuated::parse_terminated)?;
      if nested
        .iter()
        .any(|meta| matches!(meta, Meta::Path(path) if path.is_ident("C")))
      {
        return Ok(());
      }
    }
  }
  Err(Error::new(
    span,
    "cppgc inheritance requires #[repr(C)] on the type",
  ))
}

struct InheritanceList {
  ancestors: Vec<Type>,
}

impl Parse for InheritanceList {
  fn parse(input: syn::parse::ParseStream) -> Result<Self> {
    let parent = input.parse::<Type>()?;
    let mut ancestors = Vec::new();
    ancestors.push(parent);
    while input.peek(Token![=>]) {
      let _ = input.parse::<Token![=>]>()?;
      let ancestor = input.parse::<Type>()?;
      ancestors.push(ancestor);
    }
    Ok(InheritanceList { ancestors })
  }
}

fn parse_base_attr(attrs: &[Attribute]) -> Result<InheritanceList> {
  let mut found = None;
  for attr in attrs {
    if !attr.path().is_ident("cppgc_base") {
      continue;
    }
    if found.is_some() {
      return Err(Error::new(
        attr.span(),
        "cppgc_base specified more than once",
      ));
    }
    let base = attr.parse_args::<InheritanceList>()?;
    found = Some(base);
  }
  found.ok_or_else(|| {
    Error::new(
      proc_macro2::Span::call_site(),
      "derive(CppgcInherits) requires #[cppgc_base(BaseType)]",
    )
  })
}

fn parse_inheritors_attr(attrs: &[Attribute]) -> Result<Vec<Type>> {
  let mut found: Option<Vec<Type>> = None;
  for attr in attrs {
    if !attr.path().is_ident("cppgc_inheritors") {
      continue;
    }
    if found.is_some() {
      return Err(Error::new(
        attr.span(),
        "cppgc_inheritors specified more than once",
      ));
    }
    let args =
      attr.parse_args_with(Punctuated::<Type, Token![,]>::parse_terminated)?;
    found = Some(args.into_iter().collect());
  }
  found.ok_or_else(|| {
    Error::new(
      proc_macro2::Span::call_site(),
      "derive(CppgcBase) requires #[cppgc_inheritors(Type1, Type2, ...)]",
    )
  })
}

#[derive(Clone)]
enum FieldRef {
  Named(Ident, proc_macro2::Span),
  Unnamed(syn::Index, proc_macro2::Span),
}

fn first_field(data: &Data) -> Option<FieldRefWithType> {
  match data {
    Data::Struct(data_struct) => match &data_struct.fields {
      Fields::Named(fields) => fields.named.first().map(|f| FieldRefWithType {
        field: FieldRef::Named(f.ident.clone().unwrap(), f.ty.span()),
        ty: f.ty.clone(),
      }),
      Fields::Unnamed(fields) => {
        fields.unnamed.first().map(|f| FieldRefWithType {
          field: FieldRef::Unnamed(syn::Index::from(0), f.ty.span()),
          ty: f.ty.clone(),
        })
      }
      Fields::Unit => None,
    },
    _ => None,
  }
}

struct FieldRefWithType {
  field: FieldRef,
  ty: Type,
}

fn types_equal(a: &Type, b: &Type) -> bool {
  quote!(#a).to_string() == quote!(#b).to_string()
}

// Tests live in ops/tests to exercise the proc-macros.
