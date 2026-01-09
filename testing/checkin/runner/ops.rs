// Copyright 2018-2025 the Deno authors. MIT license.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use std::time::Instant;

use deno_core::CppgcBase;
use deno_core::CppgcInherits;
use deno_core::GarbageCollected;
use deno_core::OpState;
use deno_core::op2;
use deno_core::stats::RuntimeActivityDiff;
use deno_core::stats::RuntimeActivitySnapshot;
use deno_core::stats::RuntimeActivityStats;
use deno_core::stats::RuntimeActivityStatsFactory;
use deno_core::stats::RuntimeActivityStatsFilter;
use deno_core::v8;
use deno_core::v8::cppgc::GcCell;
use deno_core::v8_static_strings;
use deno_error::JsErrorBox;

use super::Output;
use super::TestData;
use super::extensions::SomeType;

pub struct StartTime(Instant);

impl Default for StartTime {
  fn default() -> Self {
    Self(Instant::now())
  }
}
impl std::ops::Deref for StartTime {
  type Target = Instant;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

fn expose_time(duration: Duration, out: &mut [u8]) {
  let seconds = duration.as_secs() as u32;
  let subsec_nanos = duration.subsec_nanos();

  if out.len() >= 8 {
    out[0..4].copy_from_slice(&seconds.to_ne_bytes());
    out[4..8].copy_from_slice(&subsec_nanos.to_ne_bytes());
  }
}

#[op2(fast)]
pub fn op_now(state: &mut OpState, #[buffer] buf: &mut [u8]) {
  let start_time = state.borrow::<StartTime>();
  let elapsed = start_time.elapsed();
  expose_time(elapsed, buf);
}

#[op2(fast)]
pub fn op_log_debug(#[string] s: &str) {
  println!("{s}");
}

#[op2(fast)]
pub fn op_log_info(state: &mut OpState, #[string] s: String) {
  println!("{s}");
  state.borrow_mut::<Output>().line(s);
}

#[op2(fast)]
pub fn op_stats_capture(#[string] name: String, state: Rc<RefCell<OpState>>) {
  let stats = state
    .borrow()
    .borrow::<RuntimeActivityStatsFactory>()
    .clone();
  let data = stats.capture(&RuntimeActivityStatsFilter::all());
  let mut state = state.borrow_mut();
  let test_data = state.borrow_mut::<TestData>();
  test_data.insert(name, data);
}

#[op2]
#[serde]
pub fn op_stats_dump(
  state: &OpState,
  #[string] name: String,
) -> RuntimeActivitySnapshot {
  let test_data = state.borrow::<TestData>();
  let stats = test_data.get::<RuntimeActivityStats>(name);
  stats.dump()
}

#[op2]
#[serde]
pub fn op_stats_diff(
  state: &OpState,
  #[string] before: String,
  #[string] after: String,
) -> RuntimeActivityDiff {
  let test_data = state.borrow::<TestData>();
  let before = test_data.get::<RuntimeActivityStats>(before);
  let after = test_data.get::<RuntimeActivityStats>(after);
  RuntimeActivityStats::diff(before, after)
}

#[op2(fast)]
pub fn op_stats_delete(state: &mut OpState, #[string] name: String) {
  state
    .borrow_mut::<TestData>()
    .take::<RuntimeActivityStats>(name);
}

#[op2(fast, no_side_effects)]
pub fn op_thing_is_string(thing: v8::Local<v8::Value>) -> bool {
  thing.is_string()
}

#[op2]
pub fn op_map<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  array: v8::Local<'a, v8::Array>,
  func: v8::Local<'a, v8::Function>,
) -> v8::Local<'a, v8::Value> {
  let len = array.length();
  let out = v8::Array::new(scope, len as i32);
  for i in 0..len {
    let value = array.get_index(scope, i as u32).unwrap();
    let result = func.call(scope, func.into(), &[value]).unwrap();
    out.set_index(scope, i as u32, result).unwrap();
  }
  out.into()
}

struct ObjectAssignFn(Rc<v8::Global<v8::Function>>);

fn get_object_assign_fn(
  state: &mut OpState,
  scope: &mut v8::PinScope,
) -> Rc<v8::Global<v8::Function>> {
  if let Some(object_assign_fn) = state.try_borrow::<ObjectAssignFn>() {
    object_assign_fn.0.clone()
  } else {
    let context = scope.get_current_context();
    let object_str = v8::String::new(scope, "Object").unwrap();
    let assign_str = v8::String::new(scope, "assign").unwrap();
    let object = context
      .global(scope)
      .get(scope, object_str.into())
      .unwrap()
      .cast::<v8::Object>();
    let assign = object
      .get(scope, assign_str.into())
      .unwrap()
      .cast::<v8::Function>();
    let global = Rc::new(v8::Global::new(scope, assign));
    state.put(ObjectAssignFn(global.clone()));
    global
  }
}

#[op2]
pub fn op_object_assign<'a>(
  state: &mut OpState,
  scope: &mut v8::PinScope<'a, '_>,
  target: v8::Local<'a, v8::Object>,
  source: v8::Local<'a, v8::Object>,
) -> v8::Local<'a, v8::Object> {
  let object_assign_fn = get_object_assign_fn(state, scope);
  let f = v8::Local::new(scope, &*object_assign_fn);
  f.call(
    scope,
    v8::undefined(scope).into(),
    &[target.into(), source.into()],
  )
  .unwrap();

  target
}

#[op2]
pub fn op_map2<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  array: v8::Local<'a, v8::Array>,
  func: v8::Local<'a, v8::Function>,
) -> v8::Local<'a, v8::Value> {
  let len = array.length();
  let mut out = smallvec::SmallVec::<[_; 16]>::with_capacity(len as usize);
  for i in 0..len {
    let value = array.get_index(scope, i as u32).unwrap();
    out.push(func.call(scope, func.into(), &[value]).unwrap());
  }
  v8::Array::new_with_elements(scope, &out).into()
}

#[op2]
pub fn op_nop(value: v8::Local<v8::Value>) -> v8::Local<v8::Value> {
  value
}

#[op2]
pub fn op_callme<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  func: v8::Local<'a, v8::Function>,
) -> v8::Local<'a, v8::Value> {
  func.call(scope, func.into(), &[]).unwrap()
}

pub struct TestObjectWrap {}

unsafe impl GarbageCollected for TestObjectWrap {
  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"TestObjectWrap"
  }
}

fn int(
  _scope: &mut v8::PinScope,
  value: v8::Local<v8::Value>,
) -> Result<(), JsErrorBox> {
  if value.is_int32() {
    return Ok(());
  }

  Err(JsErrorBox::type_error("Expected int"))
}

fn int_op(
  _scope: &mut v8::PinScope,
  args: &v8::FunctionCallbackArguments,
) -> Result<(), JsErrorBox> {
  if args.length() != 1 {
    return Err(JsErrorBox::type_error("Expected one argument"));
  }

  Ok(())
}

#[op2]
impl TestObjectWrap {
  #[constructor]
  #[cppgc]
  fn new(_: bool) -> TestObjectWrap {
    TestObjectWrap {}
  }

  #[fast]
  #[smi]
  fn with_varargs(
    &self,
    #[varargs] args: Option<&v8::FunctionCallbackArguments>,
  ) -> u32 {
    args.map(|args| args.length() as u32).unwrap_or(0)
  }

  #[fast]
  fn with_scope_fast(&self, _scope: &mut v8::PinScope) {}

  #[fast]
  #[undefined]
  fn undefined_result(&self) -> Result<(), JsErrorBox> {
    Ok(())
  }

  #[fast]
  #[rename("with_RENAME")]
  fn with_rename(&self) {}

  #[async_method]
  async fn with_async_fn(&self, #[smi] ms: u32) -> Result<(), JsErrorBox> {
    tokio::time::sleep(std::time::Duration::from_millis(ms as u64)).await;
    Ok(())
  }

  #[fast]
  #[validate(int_op)]
  fn with_validate_int(
    &self,
    #[validate(int)]
    #[smi]
    t: u32,
  ) -> Result<u32, JsErrorBox> {
    Ok(t)
  }

  #[fast]
  fn with_this(&self, #[this] _: v8::Global<v8::Object>) {}

  #[getter]
  #[string]
  fn with_slow_getter(&self) -> String {
    String::from("getter")
  }
}

#[derive(CppgcInherits)]
#[cppgc_base(DOMPointReadOnly)]
#[repr(C)]
pub struct DOMPoint {
  base: DOMPointReadOnly,
}

unsafe impl GarbageCollected for DOMPoint {
  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"DOMPoint"
  }
}

impl DOMPoint {
  fn from_point_inner(
    scope: &mut v8::PinScope,
    other: v8::Local<v8::Object>,
  ) -> Result<DOMPoint, JsErrorBox> {
    fn get(
      scope: &mut v8::PinScope,
      other: v8::Local<v8::Object>,
      key: &str,
    ) -> Option<f64> {
      let key = v8::String::new(scope, key).unwrap();
      other
        .get(scope, key.into())
        .map(|x| x.to_number(scope).unwrap().value())
    }

    Ok(DOMPoint {
      base: DOMPointReadOnly {
        x: GcCell::new(get(scope, other, "x").unwrap_or(0.0)),
        y: GcCell::new(get(scope, other, "y").unwrap_or(0.0)),
        z: GcCell::new(get(scope, other, "z").unwrap_or(0.0)),
        w: GcCell::new(get(scope, other, "w").unwrap_or(0.0)),
      },
    })
  }
}

#[derive(CppgcBase)]
#[cppgc_inheritors(DOMPoint)]
#[repr(C)]
pub struct DOMPointReadOnly {
  x: GcCell<f64>,
  y: GcCell<f64>,
  z: GcCell<f64>,
  w: GcCell<f64>,
}

unsafe impl GarbageCollected for DOMPointReadOnly {
  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"DOMPointReadOnly"
  }
}

#[op2(base)]
impl DOMPointReadOnly {
  #[constructor]
  #[cppgc]
  fn new(
    x: Option<f64>,
    y: Option<f64>,
    z: Option<f64>,
    w: Option<f64>,
  ) -> DOMPointReadOnly {
    DOMPointReadOnly {
      x: GcCell::new(x.unwrap_or(0.0)),
      y: GcCell::new(y.unwrap_or(0.0)),
      z: GcCell::new(z.unwrap_or(0.0)),
      w: GcCell::new(w.unwrap_or(0.0)),
    }
  }

  #[getter]
  fn x(&self, isolate: &v8::Isolate) -> f64 {
    *self.x.get(isolate)
  }

  #[getter]
  fn y(&self, isolate: &v8::Isolate) -> f64 {
    *self.y.get(isolate)
  }

  #[getter]
  fn z(&self, isolate: &v8::Isolate) -> f64 {
    *self.z.get(isolate)
  }

  #[getter]
  fn w(&self, isolate: &v8::Isolate) -> f64 {
    *self.w.get(isolate)
  }
}

#[op2(inherit = DOMPointReadOnly)]
impl DOMPoint {
  #[constructor]
  #[cppgc]
  fn new(
    x: Option<f64>,
    y: Option<f64>,
    z: Option<f64>,
    w: Option<f64>,
  ) -> DOMPoint {
    DOMPoint {
      base: DOMPointReadOnly {
        x: GcCell::new(x.unwrap_or(0.0)),
        y: GcCell::new(y.unwrap_or(0.0)),
        z: GcCell::new(z.unwrap_or(0.0)),
        w: GcCell::new(w.unwrap_or(0.0)),
      },
    }
  }

  #[cppgc]
  #[reentrant]
  #[required(1)]
  #[static_method]
  fn from_point(
    scope: &mut v8::PinScope,
    other: v8::Local<v8::Object>,
  ) -> Result<DOMPoint, JsErrorBox> {
    DOMPoint::from_point_inner(scope, other)
  }

  #[cppgc]
  #[reentrant]
  #[required(1)]
  fn from_point(
    &self,
    scope: &mut v8::PinScope,
    other: v8::Local<v8::Object>,
  ) -> Result<DOMPoint, JsErrorBox> {
    DOMPoint::from_point_inner(scope, other)
  }

  #[setter]
  fn x(&self, isolate: &mut v8::Isolate, x: f64) {
    self.base.x.set(isolate, x);
  }

  #[getter]
  fn x(&self, isolate: &v8::Isolate) -> f64 {
    *self.base.x.get(isolate)
  }

  #[setter]
  fn y(&self, isolate: &mut v8::Isolate, y: f64) {
    self.base.y.set(isolate, y);
  }

  #[getter]
  fn y(&self, isolate: &v8::Isolate) -> f64 {
    *self.base.y.get(isolate)
  }

  #[setter]
  fn z(&self, isolate: &mut v8::Isolate, z: f64) {
    self.base.z.set(isolate, z);
  }

  #[getter]
  fn z(&self, isolate: &v8::Isolate) -> f64 {
    *self.base.z.get(isolate)
  }

  #[setter]
  fn w(&self, isolate: &mut v8::Isolate, w: f64) {
    self.base.w.set(isolate, w);
  }

  #[getter]
  fn w(&self, isolate: &v8::Isolate) -> f64 {
    *self.base.w.get(isolate)
  }

  #[fast]
  fn wrapping_smi(&self, #[smi] t: u32) -> u32 {
    t
  }

  #[fast]
  #[symbol("symbolMethod")]
  fn with_symbol(&self) {}

  #[fast]
  #[stack_trace]
  fn with_stack_trace(&self) {}

  #[fast]
  #[rename("impl")]
  fn impl_method(&self) {}
}

#[repr(u8)]
#[derive(Clone, Copy)]
pub enum TestEnumWrap {
  #[allow(dead_code)]
  A,
}

unsafe impl GarbageCollected for TestEnumWrap {
  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"TestEnumWrap"
  }
}

#[op2]
impl TestEnumWrap {
  #[getter]
  fn as_int(&self) -> u8 {
    *self as u8
  }
}

#[op2(fast)]
pub fn op_nop_generic<T: SomeType + 'static>(state: &mut OpState) {
  state.take::<T>();
}

unsafe impl GarbageCollected for Socket {
  fn trace(&self, _visitor: &mut v8::cppgc::Visitor) {}

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"Socket"
  }
}

pub struct Socket {}

#[op2]
impl Socket {
  #[constructor]
  #[cppgc]
  fn new(_: bool) -> Socket {
    Socket {}
  }
}

v8_static_strings! {
  FOO = "foo",
}

struct FooStatic {
  foo: v8::Global<v8::String>,
}

#[op2]
pub fn op_prop_access_static<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  state: &mut OpState,
  obj: v8::Local<'a, v8::Object>,
) -> v8::Local<'a, v8::Value> {
  if let Some(foo_static) = state.try_borrow::<FooStatic>() {
    let foo = v8::Local::new(scope, &foo_static.foo);
    obj.get(scope, foo.into()).unwrap()
  } else {
    let foo = FOO.v8_string(scope).unwrap();
    state.put(FooStatic {
      foo: v8::Global::new(scope, foo),
    });
    obj.get(scope, foo.into()).unwrap()
  }
}

#[op2]
pub fn op_prop_access_static_uncached<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  obj: v8::Local<'a, v8::Object>,
) -> v8::Local<'a, v8::Value> {
  let foo = FOO.v8_string(scope).unwrap();
  obj.get(scope, foo.into()).unwrap()
}

#[op2]
pub fn op_prop_access_internalized_uncached<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  obj: v8::Local<'a, v8::Object>,
) -> v8::Local<'a, v8::Value> {
  let foo = v8::String::new_from_utf8(
    scope,
    "foo".as_bytes(),
    v8::NewStringType::Internalized,
  )
  .unwrap();
  obj.get(scope, foo.into()).unwrap()
}

macro_rules! lazy_strings {
    ($name: ident { $($field: ident = $value: expr),* }) => {
    #[derive(Clone)]
    struct $name {
      $(
    $field: Rc<v8::Global<v8::String>>,
    )* }

    impl $name {
      #[inline(always)]
      fn new<'a>(scope: &mut v8::PinScope<'a, '_>) -> Self {
        $(
          let $field = v8::String::new_from_utf8(scope, $value.as_bytes(), v8::NewStringType::Internalized).unwrap();
          let $field = Rc::new(v8::Global::new(scope, $field));
        )*
        Self {
          $(
            $field,
          )*
        }
      }

      #[inline(always)]
      fn get_or_init<'a>(scope: &mut v8::PinScope<'a, '_>, op_state: &mut deno_core::OpState) -> Self {
        if let Some(strings) = op_state.try_borrow::<Self>() {
          strings.clone()
        } else {
          let strings = Self::new(scope);
          op_state.put(strings.clone());
          strings
        }
      }

      $(
        #[inline(always)]
        fn $field<'scope>(&self, scope: &v8::PinScope<'scope, '_>) -> v8::Local<'scope, v8::String> {
          v8::Local::new(scope, &*self.$field)
        }
      )*
    }
  };
}

lazy_strings! {
  FooStrings {
    foo = "foo"
  }
}

#[op2]
pub fn op_prop_access_lazy<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  obj: v8::Local<'a, v8::Object>,
  state: &mut OpState,
) -> v8::Local<'a, v8::Value> {
  let foo_strings = FooStrings::get_or_init(scope, state);
  obj.get(scope, foo_strings.foo(scope).into()).unwrap()
}

pub struct FooGetter {
  func: v8::Global<v8::Function>,
}

impl FooGetter {
  fn call<'a>(
    &self,
    scope: &mut v8::PinScope<'a, '_>,
    obj: v8::Local<'a, v8::Object>,
  ) -> v8::Local<'a, v8::Value> {
    let func = v8::Local::new(scope, &self.func);
    func.call(scope, func.into(), &[obj.into()]).unwrap()
  }
}

#[op2(fast)]
pub fn op_set_foo_getter<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  func: v8::Local<'a, v8::Function>,
  state: &mut OpState,
) {
  let foo_getter = FooGetter {
    func: v8::Global::new(scope, func),
  };
  state.put(foo_getter);
}

#[op2]
pub fn op_get_foo_from_js<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  obj: v8::Local<'a, v8::Object>,
  state: &mut OpState,
) -> v8::Local<'a, v8::Value> {
  let foo_getter = state.borrow::<FooGetter>();
  foo_getter.call(scope, obj)
}

#[derive(Debug, thiserror::Error, deno_error::JsError)]
#[class(generic)]
enum ValidationError {
  #[error("Arg1 must be a string")]
  Arg1MustBeString,
  #[error("Options must be an object")]
  OptionsMustBeObject,
  #[error("Callback or options is required")]
  CallbackOrOptionsRequired,
  #[error("Bar must be a boolean")]
  BarMustBeBoolean,
  #[error("Baz must be a number")]
  BazMustBeNumber,
  #[error("Required is required")]
  RequiredIsRequired,
  #[error("Required must be a number")]
  RequiredMustBeNumber,
  #[error("Foo must be a string")]
  FooMustBeString,
}

impl From<ValidationError> for JsErrorBox {
  fn from(value: ValidationError) -> Self {
    JsErrorBox::from_err(value)
  }
}

lazy_strings! {
  ArgStrings {
    bar = "bar",
    baz = "baz",
    required = "required",
    foo = "foo"
  }
}

fn validate_args<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  strings: &ArgStrings,
  arg1: v8::Local<'a, v8::String>,
  callback_or_options: v8::Local<'a, v8::Value>,
  options: v8::Local<'a, v8::Value>,
) -> Result<(), JsErrorBox> {
  let arg1_value: v8::Local<v8::Value> = arg1.into();
  if !arg1_value.is_string() {
    return Err(ValidationError::Arg1MustBeString.into());
  }

  let opts = if callback_or_options.is_function() {
    options
  } else {
    callback_or_options
  };

  if !opts.is_object() || opts.is_null() {
    return Err(ValidationError::OptionsMustBeObject.into());
  }

  if opts.is_null_or_undefined() {
    if !callback_or_options.is_function() {
      return Err(ValidationError::CallbackOrOptionsRequired.into());
    }
    return Ok(());
  }

  let obj = opts.cast::<v8::Object>();

  let bar_key = strings.bar(scope);
  let bar = obj.get(scope, bar_key.into()).unwrap();
  if !bar.is_null_or_undefined() {
    if !bar.is_boolean() {
      return Err(ValidationError::BarMustBeBoolean.into());
    }
  }

  let baz_key = strings.baz(scope);
  let baz = obj.get(scope, baz_key.into()).unwrap();
  if !baz.is_null_or_undefined() {
    if !baz.is_number() {
      return Err(ValidationError::BazMustBeNumber.into());
    }
  }

  let required_key = strings.required(scope);
  if !obj.has(scope, required_key.into()).unwrap() {
    return Err(ValidationError::RequiredIsRequired.into());
  }
  let required = obj.get(scope, required_key.into()).unwrap();
  if !required.is_number() {
    return Err(ValidationError::RequiredMustBeNumber.into());
  }

  let foo_key = strings.foo(scope);
  let foo = obj.get(scope, foo_key.into()).unwrap();
  if !foo.is_null_or_undefined() {
    if !foo.is_string() {
      return Err(ValidationError::FooMustBeString.into());
    }
  }

  Ok(())
}

#[op2(fast)]
pub fn op_validate_args<'a>(
  scope: &mut v8::PinScope<'a, '_>,
  state: &mut OpState,
  arg1: v8::Local<'a, v8::String>,
  callback_or_options: v8::Local<'a, v8::Value>,
  options: v8::Local<'a, v8::Value>,
) -> Result<(), JsErrorBox> {
  let strings = ArgStrings::get_or_init(scope, state);
  validate_args(scope, &strings, arg1, callback_or_options, options)
}
