// Copyright 2018-2025 the Deno authors. MIT license.

use crate::JsRuntime;
use crate::runtime::SnapshotLoadDataStore;
use crate::runtime::SnapshotStoreDataStore;
use serde::Deserialize;
use serde::Serialize;
use std::any::TypeId;
use std::any::type_name;
use std::collections::BTreeMap;
pub use v8::cppgc::GarbageCollected;
pub use v8::cppgc::GcCell;

const CPPGC_SINGLE_TAG: u16 = 1;

#[repr(C)]
struct CppGcObject<T: GarbageCollected> {
  tag: TypeId,
  member: T,
}

unsafe impl<T: GarbageCollected> v8::cppgc::GarbageCollected
  for CppGcObject<T>
{
  fn trace(&self, visitor: &mut v8::cppgc::Visitor) {
    self.member.trace(visitor);
  }

  fn get_name(&self) -> &'static std::ffi::CStr {
    self.member.get_name()
  }
}

pub(crate) fn cppgc_template_constructor(
  _scope: &mut v8::PinScope,
  _args: v8::FunctionCallbackArguments,
  _rv: v8::ReturnValue,
) {
}

pub(crate) fn make_cppgc_template<'s, 'i>(
  scope: &mut v8::PinScope<'s, 'i, ()>,
) -> v8::Local<'s, v8::FunctionTemplate> {
  v8::FunctionTemplate::new(scope, cppgc_template_constructor)
}

#[doc(hidden)]
pub fn make_cppgc_empty_object<'a, 'i, T: GarbageCollected + 'static>(
  scope: &mut v8::PinScope<'a, 'i>,
) -> v8::Local<'a, v8::Object> {
  let state = JsRuntime::state_from(scope);
  let templates = state.function_templates.borrow();

  match templates.get::<T>() {
    Some(templ) => {
      let templ = v8::Local::new(scope, templ);
      let inst = templ.instance_template(scope);
      inst.new_instance(scope).unwrap()
    }
    _ => {
      let templ =
        v8::Local::new(scope, state.cppgc_template.borrow().as_ref().unwrap());
      let func = templ.get_function(scope).unwrap();
      func.new_instance(scope, &[]).unwrap()
    }
  }
}

pub fn make_cppgc_object<'a, 'i, T: GarbageCollected + 'static>(
  scope: &mut v8::PinScope<'a, 'i>,
  t: T,
) -> v8::Local<'a, v8::Object> {
  let obj = make_cppgc_empty_object::<T>(scope);
  wrap_object(scope, obj, t)
}

// Wrap an API object (eg: `args.This()`)
pub fn wrap_object<'a, T: GarbageCollected + 'static>(
  isolate: &mut v8::Isolate,
  obj: v8::Local<'a, v8::Object>,
  t: T,
) -> v8::Local<'a, v8::Object> {
  let heap = isolate.get_cpp_heap().unwrap();
  unsafe {
    let member = v8::cppgc::make_garbage_collected(
      heap,
      CppGcObject {
        tag: TypeId::of::<T>(),
        member: t,
      },
    );

    v8::Object::wrap::<CPPGC_SINGLE_TAG, CppGcObject<T>>(isolate, obj, &member);

    obj
  }
}

pub fn make_cppgc_proto_object<'a, 'i, T: GarbageCollected + 'static>(
  scope: &mut v8::PinScope<'a, 'i>,
  t: T,
) -> v8::Local<'a, v8::Object> {
  make_cppgc_object(scope, t)
}

pub struct UnsafePtr<T: GarbageCollected> {
  inner: v8::cppgc::UnsafePtr<CppGcObject<T>>,
  root: Option<v8::cppgc::Persistent<CppGcObject<T>>>,
}

impl<T: GarbageCollected> UnsafePtr<T> {
  #[allow(clippy::missing_safety_doc)]
  pub unsafe fn as_ref(&self) -> &T {
    unsafe { &self.inner.as_ref().member }
  }
}

#[doc(hidden)]
impl<T: GarbageCollected> UnsafePtr<T> {
  /// If this pointer is used in an async function, it could leave the stack,
  /// so this method can be called to root it in the GC and keep the reference
  /// valid.
  pub fn root(&mut self) {
    if self.root.is_none() {
      self.root = Some(v8::cppgc::Persistent::new(&self.inner));
    }
  }
}

impl<T: GarbageCollected> std::ops::Deref for UnsafePtr<T> {
  type Target = T;
  fn deref(&self) -> &T {
    &unsafe { self.inner.as_ref() }.member
  }
}

#[doc(hidden)]
#[allow(clippy::needless_lifetimes)]
fn try_unwrap_cppgc_with<'sc, T: GarbageCollected + 'static>(
  isolate: &mut v8::Isolate,
  val: v8::Local<'sc, v8::Value>,
  inheriting: &[TypeId],
) -> Option<UnsafePtr<T>> {
  let Ok(obj): Result<v8::Local<v8::Object>, _> = val.try_into() else {
    return None;
  };
  if !obj.is_api_wrapper() {
    return None;
  }

  let obj = unsafe {
    v8::Object::unwrap::<CPPGC_SINGLE_TAG, CppGcObject<T>>(isolate, obj)
  }?;

  let tag = unsafe { obj.as_ref() }.tag;
  if tag != TypeId::of::<T>() && !inheriting.contains(&tag) {
    return None;
  }

  Some(UnsafePtr {
    inner: obj,
    root: None,
  })
}

#[doc(hidden)]
#[allow(clippy::needless_lifetimes)]
pub fn try_unwrap_cppgc_object<'sc, T: GarbageCollected + 'static>(
  isolate: &mut v8::Isolate,
  val: v8::Local<'sc, v8::Value>,
) -> Option<UnsafePtr<T>> {
  try_unwrap_cppgc_with::<T>(isolate, val, &[])
}

#[doc(hidden)]
#[allow(clippy::needless_lifetimes)]
pub fn try_unwrap_cppgc_base_object<
  'sc,
  T: GarbageCollected + Base + 'static,
>(
  isolate: &mut v8::Isolate,
  val: v8::Local<'sc, v8::Value>,
) -> Option<UnsafePtr<T>> {
  try_unwrap_cppgc_with::<T>(isolate, val, T::INHERITING_TYPES)
}

pub struct Ref<T: GarbageCollected> {
  inner: v8::cppgc::Persistent<CppGcObject<T>>,
}

impl<T: GarbageCollected> std::ops::Deref for Ref<T> {
  type Target = T;
  fn deref(&self) -> &T {
    &self.inner.get().unwrap().member
  }
}

#[doc(hidden)]
#[allow(clippy::needless_lifetimes)]
pub fn try_unwrap_cppgc_persistent_object<
  'sc,
  T: GarbageCollected + 'static,
>(
  isolate: &mut v8::Isolate,
  val: v8::Local<'sc, v8::Value>,
) -> Option<Ref<T>> {
  let ptr = try_unwrap_cppgc_object::<T>(isolate, val)?;
  Some(Ref {
    inner: v8::cppgc::Persistent::new(&ptr.inner),
  })
}

#[doc(hidden)]
#[allow(clippy::needless_lifetimes)]
pub fn try_unwrap_cppgc_base_persistent_object<
  'sc,
  T: GarbageCollected + Base + 'static,
>(
  isolate: &mut v8::Isolate,
  val: v8::Local<'sc, v8::Value>,
) -> Option<Ref<T>> {
  let ptr = try_unwrap_cppgc_base_object::<T>(isolate, val)?;
  Some(Ref {
    inner: v8::cppgc::Persistent::new(&ptr.inner),
  })
}

pub struct Member<T: GarbageCollected> {
  inner: v8::cppgc::Member<CppGcObject<T>>,
}

impl<T: GarbageCollected> From<Ref<T>> for Member<T> {
  fn from(value: Ref<T>) -> Self {
    Member {
      inner: v8::cppgc::Member::new(&value.inner),
    }
  }
}

impl<T: GarbageCollected> std::ops::Deref for Member<T> {
  type Target = T;
  fn deref(&self) -> &T {
    &unsafe { self.inner.get().unwrap() }.member
  }
}

impl<T: GarbageCollected> v8::cppgc::Traced for Member<T> {
  fn trace(&self, visitor: &mut v8::cppgc::Visitor) {
    visitor.trace(&self.inner);
  }
}

#[derive(Default)]
pub struct FunctionTemplateData {
  store: BTreeMap<String, v8::Global<v8::FunctionTemplate>>,
}

#[derive(Default, Serialize, Deserialize)]
pub struct FunctionTemplateSnapshotData {
  store_handles: Vec<(String, u32)>,
}

impl FunctionTemplateData {
  pub fn insert(
    &mut self,
    key: String,
    value: v8::Global<v8::FunctionTemplate>,
  ) {
    self.store.insert(key, value);
  }

  fn get<T>(&self) -> Option<&v8::Global<v8::FunctionTemplate>> {
    self.store.get(type_name::<T>())
  }

  pub fn get_raw(
    &self,
    key: &str,
  ) -> Option<&v8::Global<v8::FunctionTemplate>> {
    self.store.get(key)
  }

  pub fn serialize_for_snapshotting(
    self,
    data_store: &mut SnapshotStoreDataStore,
  ) -> FunctionTemplateSnapshotData {
    FunctionTemplateSnapshotData {
      store_handles: self
        .store
        .into_iter()
        .map(|(k, v)| (k, data_store.register(v)))
        .collect(),
    }
  }

  pub fn update_with_snapshotted_data(
    &mut self,
    scope: &mut v8::PinScope,
    data_store: &mut SnapshotLoadDataStore,
    data: FunctionTemplateSnapshotData,
  ) {
    self.store = data
      .store_handles
      .into_iter()
      .map(|(k, v)| (k, data_store.get::<v8::FunctionTemplate>(scope, v)))
      .collect();
  }
}

#[derive(Debug)]
pub struct SameObject<T: GarbageCollected + 'static> {
  cell: std::cell::OnceCell<v8::Global<v8::Object>>,
  _phantom_data: std::marker::PhantomData<T>,
}

impl<T: GarbageCollected + 'static> SameObject<T> {
  #[allow(clippy::new_without_default)]
  pub fn new() -> Self {
    Self {
      cell: Default::default(),
      _phantom_data: Default::default(),
    }
  }
  pub fn get<F>(&self, scope: &mut v8::PinScope, f: F) -> v8::Global<v8::Object>
  where
    F: FnOnce(&mut v8::PinScope) -> T,
  {
    self
      .cell
      .get_or_init(|| {
        let v = f(scope);
        let obj = make_cppgc_object(scope, v);
        v8::Global::new(scope, obj)
      })
      .clone()
  }

  pub fn set(
    &self,
    scope: &mut v8::PinScope,
    value: T,
  ) -> Result<(), v8::Global<v8::Object>> {
    let obj = make_cppgc_object(scope, value);
    self.cell.set(v8::Global::new(scope, obj))
  }

  pub fn try_unwrap(&self, scope: &mut v8::PinScope) -> Option<UnsafePtr<T>> {
    let obj = self.cell.get()?;
    let val = v8::Local::new(scope, obj);
    try_unwrap_cppgc_object(scope, val.cast())
  }
}

pub unsafe trait Inherits<T: GarbageCollected + 'static>:
  GarbageCollected + 'static
{
}
pub unsafe trait Base: GarbageCollected + 'static {
  const INHERITING_TYPES: &[TypeId];
}

#[cfg(test)]
mod tests {
  use super::*;
  use deno_ops::{CppgcBase, CppgcInherits};
  use std::any::TypeId;

  #[repr(C)]
  #[derive(CppgcBase)]
  #[cppgc_inheritors(Derived, Derived2)]
  struct BaseType {
    _value: u8,
  }

  unsafe impl GarbageCollected for BaseType {
    fn trace(&self, _: &mut v8::cppgc::Visitor) {}

    fn get_name(&self) -> &'static std::ffi::CStr {
      c"BaseType"
    }
  }

  #[repr(C)]
  #[derive(CppgcInherits, CppgcBase)]
  #[cppgc_inheritors(Derived2)]
  #[cppgc_base(BaseType)]
  struct Derived {
    base: BaseType,
    _extra: u8,
  }

  unsafe impl GarbageCollected for Derived {
    fn trace(&self, _: &mut v8::cppgc::Visitor) {}

    fn get_name(&self) -> &'static std::ffi::CStr {
      c"DerivedType"
    }
  }

  #[test]
  fn inheriting_types_list_contains_derived() {
    assert!(BaseType::INHERITING_TYPES.contains(&TypeId::of::<Derived>()));
    assert!(BaseType::INHERITING_TYPES.contains(&TypeId::of::<Derived2>()));
    assert!(Derived::INHERITING_TYPES.contains(&TypeId::of::<Derived2>()));
  }

  unsafe impl GarbageCollected for Derived2 {
    fn trace(&self, _: &mut v8::cppgc::Visitor) {}

    fn get_name(&self) -> &'static std::ffi::CStr {
      c"Derived2"
    }
  }

  #[repr(C)]
  #[derive(CppgcInherits)]
  #[cppgc_base(Derived => BaseType)]
  struct Derived2 {
    base: Derived,
    _value: u8,
  }
}
