// Copyright 2018-2025 the Deno authors. MIT license.

use std::ops::Range;

use crate::op2::signature::*;
use proc_macro_rules::rules;

use quote::ToTokens;

use syn::ReturnType;

use syn::Type;
use syn::TypeParamBound;
use syn::TypePath;

/// One level of type unwrapping for a return value. We cannot rely on `proc-macro-rules` to correctly
/// unwrap `impl Future<...>`, so we do it by hand.
enum UnwrappedReturn {
  Type(Type),
  Result(Type),
  Future(Type),
}

pub struct GenericType {
  pub name: syn::Path,
  pub generics:
    syn::punctuated::Punctuated<syn::GenericArgument, syn::Token![,]>,
}

impl GenericType {
  fn type_arg(&self, idx: usize) -> Option<&syn::Type> {
    if let Some(syn::GenericArgument::Type(ty)) = &self.generics.get(idx) {
      Some(ty)
    } else {
      None
    }
  }
}

fn parse_generic_type(
  ty: &TypePath,
  name: &str,
  num_generics: Range<usize>,
) -> Option<GenericType> {
  if let Some(segment) = ty.path.segments.last() {
    if segment.ident.to_string() != name {
      return None;
    }
    let (count, args) = match &segment.arguments {
      syn::PathArguments::None => (0, Default::default()),
      syn::PathArguments::AngleBracketed(angle) => {
        (angle.args.len(), angle.args.clone())
      }
      syn::PathArguments::Parenthesized(_) => return None,
    };
    if !num_generics.contains(&count) {
      return None;
    }
    Some(GenericType {
      name: ty.path.clone(),
      generics: args,
    })
  } else {
    None
  }
}

fn unwrap_return(ty: &Type) -> Result<UnwrappedReturn, RetError> {
  match ty {
    Type::ImplTrait(imp) => {
      if imp.bounds.len() != 1 {
        return Err(RetError::InvalidType(ArgError::InvalidType(
          stringify_token(ty),
          "for impl trait bounds",
        )));
      }
      if let Some(TypeParamBound::Trait(t)) = imp.bounds.first() {
        rules!(t.into_token_stream() => {
          ($($_package:ident ::)* Future < Output = $ty:ty $(,)? >) => Ok(UnwrappedReturn::Future(ty)),
          ($ty:ty) => Err(RetError::InvalidType(ArgError::InvalidType(stringify_token(ty), "for impl Future"))),
        })
      } else {
        Err(RetError::InvalidType(ArgError::InvalidType(
          stringify_token(ty),
          "for impl",
        )))
      }
    }
    Type::Path(ty_path) => {
      if let Some(result) = parse_generic_type(ty_path, "Result", 1..3) {
        if let Some(arg) = result.type_arg(0) {
          Ok(UnwrappedReturn::Result(arg.clone()))
        } else {
          Ok(UnwrappedReturn::Type(ty.clone()))
        }
      } else {
        Ok(UnwrappedReturn::Type(ty.clone()))
      }
    }
    Type::Tuple(_) => Ok(UnwrappedReturn::Type(ty.clone())),
    Type::Ptr(_) => Ok(UnwrappedReturn::Type(ty.clone())),
    Type::Reference(_) => Ok(UnwrappedReturn::Type(ty.clone())),
    _ => Err(RetError::InvalidType(ArgError::InvalidType(
      stringify_token(ty),
      "for return type",
    ))),
  }
}

pub(crate) fn parse_return(
  is_async: bool,
  attrs: Attributes,
  rt: &ReturnType,
) -> Result<RetVal, RetError> {
  use UnwrappedReturn::*;

  let res = match rt {
    ReturnType::Default => RetVal::Infallible(Arg::Void),
    ReturnType::Type(_, rt) => match unwrap_return(rt)? {
      Type(ty) => RetVal::Infallible(parse_type(Position::RetVal, attrs, &ty)?),
      Result(ty) => match unwrap_return(&ty)? {
        Type(ty) => RetVal::Result(parse_type(Position::RetVal, attrs, &ty)?),
        Future(ty) => match unwrap_return(&ty)? {
          Type(ty) => {
            RetVal::ResultFuture(parse_type(Position::RetVal, attrs, &ty)?)
          }
          Result(ty) => RetVal::ResultFutureResult(parse_type(
            Position::RetVal,
            attrs,
            &ty,
          )?),
          _ => {
            return Err(RetError::InvalidType(ArgError::InvalidType(
              stringify_token(rt),
              "for result of future",
            )))
          }
        },
        _ => {
          return Err(RetError::InvalidType(ArgError::InvalidType(
            stringify_token(rt),
            "for result",
          )))
        }
      },
      Future(ty) => match unwrap_return(&ty)? {
        Type(ty) => RetVal::Future(parse_type(Position::RetVal, attrs, &ty)?),
        Result(ty) => {
          RetVal::FutureResult(parse_type(Position::RetVal, attrs, &ty)?)
        }
        _ => {
          return Err(RetError::InvalidType(ArgError::InvalidType(
            stringify_token(rt),
            "for future",
          )))
        }
      },
    },
  };

  // If the signature was async, wrap this return value in one level of future.
  if is_async {
    let res = match res {
      RetVal::Infallible(t) => RetVal::Future(t),
      RetVal::Result(t) => RetVal::FutureResult(t),
      _ => {
        return Err(RetError::InvalidType(ArgError::InvalidType(
          stringify_token(rt),
          "for async return",
        )))
      }
    };
    Ok(res)
  } else {
    Ok(res)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use syn::parse_str;

  #[test]
  fn test_parse_result() {
    use Arg::*;
    use RetVal::*;

    for (expected, input) in [
      (Infallible(Void), "()"),
      (Result(Void), "Result<()>"),
      (Result(Void), "Result<(), ()>"),
      (Result(Void), "Result<(), (),>"),
      (Future(Void), "impl Future<Output = ()>"),
      (FutureResult(Void), "impl Future<Output = Result<()>>"),
      (ResultFuture(Void), "Result<impl Future<Output = ()>>"),
      (
        ResultFutureResult(Void),
        "Result<impl Future<Output = Result<()>>>",
      ),
    ] {
      let rt = parse_str::<ReturnType>(&format!("-> {input}"))
        .expect("Failed to parse");
      let actual = parse_return(false, Attributes::default(), &rt)
        .expect("Failed to parse return");
      assert_eq!(expected, actual);
    }
  }
}
