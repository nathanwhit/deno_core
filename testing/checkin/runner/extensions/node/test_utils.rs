use std::cell::RefCell;
use std::rc::Rc;

use super::internalized;
use deno_core::GarbageCollected;
use deno_core::JsRuntime;
use deno_core::RequestedModuleType;
use deno_core::ToV8;
use deno_core::v8;
use deno_error::JsErrorBox;

pub fn import_from(
  runtime: &mut JsRuntime,
  specifier: &str,
  name: &str,
) -> Result<v8::Global<v8::Value>, JsErrorBox> {
  let namespace = runtime
    .get_module_namespace_by_name(specifier, RequestedModuleType::None)
    .map_err(JsErrorBox::from_err)?;
  deno_core::scope!(scope, runtime);
  let namespace = v8::Local::new(scope, namespace);
  let value = namespace
    .get(scope, internalized(scope, name).into())
    .unwrap();
  Ok(v8::Global::new(scope, value))
}

fn cast_fn<
  F: for<'a, 'b, 'c> Fn(
    &'a mut v8::PinScope<'b, 'c>,
    v8::FunctionCallbackArguments<'b>,
    v8::ReturnValue<'b>,
  ),
>(
  f: F,
) -> F {
  f
}

pub trait ToArgs<'a>: Sized {
  fn to_args(
    self,
    scope: &mut v8::PinScope<'a, '_>,
  ) -> Vec<v8::Local<'a, v8::Value>>;
}

macro_rules! impl_to_args_for_tuples {
    ($($len: expr; ($($name: ident),*)),+) => {
      $(

        impl<'a, $($name),+> ToArgs<'a> for ($($name,)+)
        where
          $($name: ToV8<'a>,)+
        {
          fn to_args(
            self,
            scope: &mut v8::PinScope<'a, '_>,
          ) -> Vec<v8::Local<'a, v8::Value>> {
            #[allow(non_snake_case)]
            let ($($name,)+) = self;
            vec![
              $($name.to_v8(scope).unwrap().into()),+
            ]
          }
        }
      )+
    };
  }

impl<'a> ToArgs<'a> for &[v8::Local<'a, v8::Value>] {
  fn to_args(
    self,
    _scope: &mut v8::PinScope<'a, '_>,
  ) -> Vec<v8::Local<'a, v8::Value>> {
    self.to_vec()
  }
}

impl<'a, const N: usize> ToArgs<'a> for &[v8::Local<'a, v8::Value>; N] {
  fn to_args(
    self,
    _scope: &mut v8::PinScope<'a, '_>,
  ) -> Vec<v8::Local<'a, v8::Value>> {
    self.to_vec()
  }
}

impl<'a, T: ToV8<'a>> ToArgs<'a> for Vec<T> {
  fn to_args(
    self,
    scope: &mut v8::PinScope<'a, '_>,
  ) -> Vec<v8::Local<'a, v8::Value>> {
    self.into_iter().map(|x| x.to_v8(scope).unwrap()).collect()
  }
}

impl_to_args_for_tuples!(
  1; (A),
  2; (A, B),
  3; (A, B, C),
  4; (A, B, C, D),
  5; (A, B, C, D, E),
  6; (A, B, C, D, E, F),
  7; (A, B, C, D, E, F, G),
  8; (A, B, C, D, E, F, G, H),
  9; (A, B, C, D, E, F, G, H, I),
  10; (A, B, C, D, E, F, G, H, I, J),
  11; (A, B, C, D, E, F, G, H, I, J, K),
  12; (A, B, C, D, E, F, G, H, I, J, K, L)
);

// impl ToArgs for {

struct CallbackData<T> {
  data: RefCell<T>,
  callback: RefCell<
    Box<
      dyn for<'a> FnMut(
        &mut v8::PinScope<'a, '_>,
        &mut T,
        v8::FunctionCallbackArguments<'a>,
        v8::ReturnValue<'a>,
      ),
    >,
  >,
}

pub fn js_callback<
  's,
  T: 'static,
  F: for<'a> FnMut(
      &mut v8::PinScope<'a, '_>,
      &mut T,
      v8::FunctionCallbackArguments<'a>,
      v8::ReturnValue<'a>,
    ) + 'static,
>(
  scope: &mut v8::PinScope<'s, '_>,
  data: T,
  f: F,
) -> v8::Local<'s, v8::Function> {
  v8::FunctionBuilder::<v8::Function>::new(cast_fn(|scope, args, rv| {
    let data = get_extra_data::<CallbackData<T>>(scope, args.data());
    let mut callback = data.callback.borrow_mut();
    callback(scope, &mut *data.data.borrow_mut(), args, rv);
  }))
  .data(with_extra_data(
    scope,
    CallbackData {
      data: RefCell::new(data),
      callback: RefCell::new(Box::new(f)),
    },
  ))
  .build(scope)
  .unwrap()
}

pub struct JsObject {
  obj: Rc<v8::Global<v8::Object>>,
}

impl JsObject {
  pub fn construct<'s>(
    scope: &v8::PinScope<'s, '_>,
    constructor: v8::Local<'s, v8::Function>,
    args: &[v8::Local<'s, v8::Value>],
  ) -> Self {
    JsObject::new(scope, constructor.new_instance(scope, args).unwrap())
  }

  pub fn new(scope: &v8::PinScope, obj: v8::Local<v8::Object>) -> Self {
    Self {
      obj: Rc::new(v8::Global::new(scope, obj)),
    }
  }

  #[allow(dead_code)]
  pub fn get<'s>(
    &self,
    scope: &v8::PinScope<'s, '_>,
    name: &str,
  ) -> v8::Local<'s, v8::Value> {
    v8::Local::new(scope, &*self.obj)
      .get(scope, internalized(scope, name).into())
      .unwrap()
  }

  pub fn call<'s>(
    &self,
    scope: &mut v8::PinScope<'s, '_>,
    name: &str,
    args: impl ToArgs<'s>,
  ) -> v8::Local<'s, v8::Value> {
    let local_obj = v8::Local::new(scope, &*self.obj);
    let args = args.to_args(scope);
    local_obj
      .get(scope, internalized(scope, name).into())
      .unwrap()
      .cast::<v8::Function>()
      .call(scope, local_obj.into(), &args)
      .unwrap()
  }
}

fn get_extra_data<'s, T: 'static>(
  scope: &mut v8::PinScope<'s, '_>,
  value: v8::Local<v8::Value>,
) -> Rc<T> {
  let extra_data =
    deno_core::cppgc::try_unwrap_cppgc_object::<ExtraData<T>>(scope, value)
      .unwrap();
  unsafe { extra_data.as_ref() }.data.clone()
}

fn with_extra_data<'s, T: 'static>(
  scope: &mut v8::PinScope<'s, '_>,
  data: T,
) -> v8::Local<'s, v8::Value> {
  let extra_data = ExtraData {
    data: Rc::new(data),
  };
  let obj = deno_core::cppgc::make_cppgc_object(scope, extra_data);
  obj.into()
}

struct ExtraData<T> {
  data: Rc<T>,
}

unsafe impl<T> GarbageCollected for ExtraData<T> {
  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"ExtraData"
  }
}
