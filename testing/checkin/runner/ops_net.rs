use deno_core::AsyncRefCell;
use deno_core::CancelFuture;
use deno_core::CancelHandle;
use deno_core::ExternalOpsTracker;
use deno_core::OpState;
use deno_core::ToV8;
use deno_core::convert::Uint8Array;
use deno_core::op2;
use deno_core::v8;
use deno_core::v8::cppgc::Traced;
use deno_error::JsErrorBox;
use futures::FutureExt;
use std::cell::RefCell;
use std::ops::DerefMut;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::task::ready;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufWriter;

fn is_ipv4(s: &str) -> bool {
  std::net::Ipv4Addr::from_str(s).is_ok()
}

fn is_ipv6(s: &str) -> bool {
  std::net::Ipv6Addr::from_str(s).is_ok()
}

fn is_ip(s: &str) -> u8 {
  match std::net::IpAddr::from_str(s) {
    Ok(std::net::IpAddr::V4(_)) => 4,
    Ok(std::net::IpAddr::V6(_)) => 6,
    Err(_) => 0,
  }
}

#[inline(always)]
fn map_valid_utf8<R>(
  scope: &mut v8::Isolate,
  string: v8::Local<v8::String>,
  f: impl FnOnce(&str) -> R,
) -> Option<R> {
  let view = v8::ValueView::new(scope, string);
  match view.data() {
    v8::ValueViewData::OneByte(onebyte) => {
      std::str::from_utf8(onebyte).map(f).ok()
    }
    v8::ValueViewData::TwoByte(items) => {
      if !string.is_external_twobyte() {
        return None;
      }
      String::from_utf16(items).map(|s| f(&s)).ok()
    }
  }
}

#[op2(fast)]
pub fn op_is_ipv4(scope: &mut v8::Isolate, ip: v8::Local<v8::String>) -> bool {
  map_valid_utf8(scope, ip, is_ipv4).unwrap_or(false)
}

#[op2(fast)]
pub fn op_is_ipv6(scope: &mut v8::Isolate, ip: v8::Local<v8::String>) -> bool {
  map_valid_utf8(scope, ip, is_ipv6).unwrap_or(false)
}

#[op2(fast)]
#[smi]
pub fn op_is_ip(scope: &mut v8::Isolate, ip: v8::Local<v8::String>) -> u8 {
  map_valid_utf8(scope, ip, is_ip).unwrap_or(0)
}

struct ConnectedState {
  connected: AtomicBool,
  notify: tokio::sync::Notify,
}
impl ConnectedState {
  fn new() -> Self {
    Self {
      connected: AtomicBool::new(false),
      notify: tokio::sync::Notify::new(),
    }
  }
}
impl ConnectedState {
  fn set_connected(&self, connected: bool) {
    self
      .connected
      .store(connected, std::sync::atomic::Ordering::Relaxed);
    self.notify.notify_waiters();
  }

  async fn wait_for_connected(&self) {
    if self.connected.load(std::sync::atomic::Ordering::Relaxed) {
      return;
    }
    self.notify.notified().await;
  }
}

struct RefTrackerInner {
  ops_tracker: ExternalOpsTracker,
  refed: AtomicBool,
}

#[derive(Clone)]
struct RefTracker(Arc<RefTrackerInner>);

impl RefTracker {
  fn new(ops_tracker: ExternalOpsTracker) -> Self {
    Self(Arc::new(RefTrackerInner {
      ops_tracker,
      refed: AtomicBool::new(false),
    }))
  }

  fn ref_(&self) {
    if !self
      .0
      .refed
      .swap(true, std::sync::atomic::Ordering::Relaxed)
    {
      self.0.ops_tracker.ref_op();
    }
  }

  fn unref(&self) {
    if self
      .0
      .refed
      .swap(false, std::sync::atomic::Ordering::Relaxed)
    {
      self.0.ops_tracker.unref_op();
      eprintln!("unrefed");
    }
  }
}

struct SocketCbInner {
  write: Rc<AsyncRefCell<Option<tokio::net::tcp::OwnedWriteHalf>>>,
  read: Rc<AsyncRefCell<Option<tokio::net::tcp::OwnedReadHalf>>>,
  cb: RefCell<Option<Rc<v8::TracedReference<v8::Function>>>>,
  context: Rc<v8::Global<v8::Context>>,
  host: String,
  port: u16,
  this: Rc<v8::TracedReference<v8::Object>>,

  super_cons: SocketConstructor,
  ref_tracker: RefTracker,
  cancel: Rc<CancelHandle>,
  connected: Rc<ConnectedState>,
  scope_holder: ScopeHolder,

  reading: AtomicBool,
}

pub struct SocketCb {
  inner: Rc<SocketCbInner>,
}

unsafe impl deno_core::GarbageCollected for SocketCb {
  fn trace(&self, visitor: &mut v8::cppgc::Visitor) {
    if let Some(cb) = self.inner.cb.borrow().as_ref() {
      cb.trace(visitor);
    }
    self.inner.this.trace(visitor);
  }

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"SocketCb"
  }
}

#[op2]
pub fn op_set_duplex_constructor(
  op_state: &mut OpState,
  #[global] cons: v8::Global<v8::Function>,
) {
  op_state.put(SocketConstructor(Rc::new(cons)));
}

#[derive(Clone)]
struct SocketConstructor(Rc<v8::Global<v8::Function>>);

struct ScopeHolder(v8::UnsafeRawIsolatePtr, Rc<v8::Global<v8::Context>>);

impl ScopeHolder {
  pub fn new(
    scope: v8::UnsafeRawIsolatePtr,
    context: Rc<v8::Global<v8::Context>>,
  ) -> Self {
    Self(scope, context)
  }
}

#[op2]
impl SocketCb {
  #[constructor]
  #[cppgc]
  #[reentrant]
  pub fn new(
    #[this] me: v8::Global<v8::Object>,
    scope: &mut v8::PinScope,
    op_state: Rc<RefCell<OpState>>,
    #[string] host: String,
    #[smi] port: u16,
  ) -> Result<SocketCb, JsErrorBox> {
    let context = v8::Global::new(scope, scope.get_current_context());

    let (ops_tracker, super_cons) = {
      let op_state = op_state.borrow();
      (
        op_state.external_ops_tracker.clone(),
        op_state.borrow::<SocketConstructor>().clone(),
      )
    };

    let local_me = v8::Local::new(&scope, &me);
    let cons = v8::Local::new(scope, &*super_cons.0);
    let this = Rc::new(v8::TracedReference::new(scope, local_me));

    cons.call(scope, local_me.into(), &[]).unwrap();
    let context = Rc::new(context);
    let scope = unsafe { scope.as_raw_isolate_ptr() };
    let scope_holder = ScopeHolder::new(scope, context.clone());

    let cb = SocketCb {
      inner: Rc::new(SocketCbInner {
        write: Rc::new(AsyncRefCell::new(None)),
        read: Rc::new(AsyncRefCell::new(None)),
        cb: RefCell::new(None),
        context,
        cancel: Rc::new(CancelHandle::new()),
        host,
        port,
        this,
        super_cons,
        ref_tracker: RefTracker::new(ops_tracker),
        connected: Rc::new(ConnectedState::new()),
        scope_holder,
        reading: AtomicBool::new(false),
      }),
    };
    Ok(cb)
  }

  #[async_method]
  pub fn connect<'a, 'b>(
    &self,
    scope: v8::UnsafeRawIsolatePtr,
    #[global] cb: v8::Global<v8::Function>,
  ) -> impl Future<Output = Result<(), JsErrorBox>> {
    let scope_holder = ScopeHolder::new(scope, self.inner.context.clone());
    let inner = self.inner.clone();
    {
      let mut raw_isolate =
        unsafe { v8::Isolate::from_raw_isolate_ptr_unchecked(scope) };
      v8::scope!(let scope, &mut raw_isolate);
      let context = v8::Local::new(scope, &*self.inner.context);
      let scope = &mut v8::ContextScope::new(scope, context);
      let cb = v8::Local::new(scope, &cb);
      inner
        .cb
        .borrow_mut()
        .replace(Rc::new(v8::TracedReference::new(scope, cb)));
    }
    inner.ref_tracker.ref_();
    async move {
      let stream =
        tokio::net::TcpStream::connect((inner.host.as_str(), inner.port))
          .await
          .map_err(JsErrorBox::from_err)
          .unwrap();
      let (read, write) = stream.into_split();
      *inner.write.borrow_mut().await = Some(write);
      *inner.read.borrow_mut().await = Some(read);
      inner.connected.set_connected(true);
      // deno_core::unsync::spawn(async move {
      //   let cb = inner.cb.borrow().as_ref().unwrap().clone();
      //   let this = this.clone();

      //   let mut buf_reader = tokio::io::BufReader::new(read);
      //   let mut last = 0;
      //   while let Ok(buf) = buf_reader.fill_buf().await {
      //     // eprintln!("got data");
      //     if buf.is_empty() {
      //       // eprintln!("empty buf!");
      //       break;
      //     }
      //     let nread = buf.len();
      //     let data = Uint8Array(buf.to_vec());

      //     let len = buf.len();

      //     if buf.len() > 1024 || buf.len() == last {
      //       // if len_is_last {
      //       //   eprintln!("len is last: {len}");
      //       // }
      //       buf_reader.consume(nread);
      //       // eprintln!("CALLING CB");
      //       {
      //         let mut raw_isolate = unsafe {
      //           v8::Isolate::from_raw_isolate_ptr_unchecked(scope_holder.0)
      //         };
      //         v8::scope!(let scope, &mut raw_isolate);
      //         let context = v8::Local::new(scope, &*scope_holder.1);
      //         let scope = &mut v8::ContextScope::new(scope, context);
      //         let this = v8::Local::new(scope, &*this);
      //         let cb = cb.get(scope).unwrap();
      //         let data_v8 = data.to_v8(scope).unwrap();
      //         let _result =
      //           cb.call(scope, this.into(), &[data_v8.into()]).unwrap();
      //       };
      //     }

      //     last = len;
      //   }
      //   ops_tracker.unref();
      //   // eprintln!("Disconnected");
      // });

      // ops_tracker.unref_op();
      Ok(())
    }
  }

  #[fast]
  fn _read(&self, #[this] me: v8::Global<v8::Object>) {
    // eprintln!("read");
    // If we're already reading, don't start another read or set kSync
    // This prevents kSync from being set while the async task is calling push()
    if self
      .inner
      .reading
      .load(std::sync::atomic::Ordering::Relaxed)
    {
      return;
    }

    self.start_read(Rc::new(me));
  }

  #[fast]
  fn r#ref(&self) {
    self.inner.ref_tracker.ref_();
  }

  #[fast]
  fn unref(&self) {
    self.inner.ref_tracker.unref();
  }

  #[async_method]
  pub async fn write(
    &self,
    #[buffer(copy)] data: Vec<u8>,
  ) -> Result<(), JsErrorBox> {
    self
      .inner
      .write
      .borrow_mut()
      .await
      .deref_mut()
      .as_mut()
      .unwrap()
      .write_all(&data)
      .or_cancel(self.inner.cancel.clone())
      .await?
      .map_err(JsErrorBox::from_err)
  }
}

impl SocketCb {
  fn start_read(&self, this: Rc<v8::Global<v8::Object>>) {
    if self
      .inner
      .reading
      .swap(true, std::sync::atomic::Ordering::Relaxed)
    {
      return;
    }
    let inner = self.inner.clone();
    deno_core::unsync::spawn(async move {
      inner.connected.wait_for_connected().await;

      let this = this.clone();
      let mut read = inner.read.borrow_mut().await;
      let read = read.deref_mut().as_mut().unwrap();

      let mut buf = vec![0; 64 * 1024];

      while let Ok(nread) = read.read(&mut buf).await {
        if nread == 0 {
          // Push EOF (null)
          {
            let mut raw_isolate = unsafe {
              v8::Isolate::from_raw_isolate_ptr_unchecked(inner.scope_holder.0)
            };
            v8::scope!(let scope, &mut raw_isolate);
            let context = v8::Local::new(scope, &*inner.scope_holder.1);
            let scope = &mut v8::ContextScope::new(scope, context);
            let this = v8::Local::new(scope, &*this);
            let push = internalized(scope, "_pushFromAsync");
            let func =
              this.get(scope, push.into()).unwrap().cast::<v8::Function>();
            let result = func
              .call(scope, this.into(), &[v8::null(scope).into()])
              .unwrap();
            let result = result.cast::<v8::Boolean>();
            if result.is_false() {
              break;
            }
          }
          break;
        }

        if nread > 0 {
          let mut raw_isolate = unsafe {
            v8::Isolate::from_raw_isolate_ptr_unchecked(inner.scope_holder.0)
          };
          v8::scope!(let scope, &mut raw_isolate);
          let context = v8::Local::new(scope, &*inner.scope_holder.1);
          let scope = &mut v8::ContextScope::new(scope, context);
          let this = v8::Local::new(scope, &*this);
          let data = Uint8Array(buf[..nread].to_vec());
          let push = internalized(scope, "_pushFromAsync");
          let func =
            this.get(scope, push.into()).unwrap().cast::<v8::Function>();
          let arg = data.to_v8(scope).map_err(JsErrorBox::from_err).unwrap();
          let result = func.call(scope, this.into(), &[arg]).unwrap();
          let result = result.cast::<v8::Boolean>();
          if result.is_false() {
            break;
          }
        }
      }
      // inner.ref_tracker.unref();
      inner
        .reading
        .store(false, std::sync::atomic::Ordering::Relaxed);
    });
  }

  // pub fn push_data()
}

struct CallOnToV8<T> {
  do_it: bool,
  recv: v8::Global<v8::Object>,
  arg: T,
}

fn internalized<'a>(
  scope: &v8::PinScope<'a, '_>,
  s: &str,
) -> v8::Local<'a, v8::String> {
  v8::String::new_from_one_byte(
    scope,
    s.as_bytes(),
    v8::NewStringType::Internalized,
  )
  .unwrap()
}

impl<'a, T: ToV8<'a>> ToV8<'a> for CallOnToV8<T> {
  type Error = JsErrorBox;
  fn to_v8<'i>(
    self,
    scope: &mut v8::PinScope<'a, 'i>,
  ) -> Result<v8::Local<'a, v8::Value>, Self::Error> {
    if !self.do_it {
      return Ok(v8::undefined(scope).into());
    }
    let recv = self.recv.to_local(scope);
    let readable = internalized(scope, "readable");
    let readable_obj = recv
      .get(scope, readable.into())
      .unwrap()
      .cast::<v8::Object>();

    let push = internalized(scope, "push");

    eprintln!("calling!");
    let func = readable_obj
      .get(scope, push.into())
      .unwrap()
      .cast::<v8::Function>();
    let arg = self.arg.to_v8(scope).map_err(JsErrorBox::from_err)?;
    func.call(scope, recv.into(), &[arg]).unwrap();
    Ok(v8::undefined(scope).into())
  }
}

trait Ext<T> {
  fn to_local<'a>(self, scope: &v8::PinScope<'a, '_>) -> v8::Local<'a, T>;
}

impl<'b, T, H> Ext<T> for H
where
  H: v8::Handle<Data = T> + 'b,
{
  fn to_local<'a>(self, scope: &v8::PinScope<'a, '_>) -> v8::Local<'a, T> {
    v8::Local::new(scope, self)
  }
}

fn to_local<'a, T>(
  scope: &v8::PinScope<'a, '_>,
  global: impl v8::Handle<Data = T>,
) -> v8::Local<'a, T> {
  v8::Local::new(scope, global)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_is_ipv4() {
    let pass_cases = [
      "127.0.0.1",
      "192.168.1.1",
      "10.0.0.1",
      "172.16.0.1",
      "172.31.255.255",
      "100.64.0.1",
      "100.127.255.255",
      "169.254.0.1",
      "169.254.255.255",
    ];
    let fail_cases = [
      "127.0.0.1.1",
      "127.0.0.0.1",
      "127.0.0.256",
      "127.0.0.-1",
      "127.0.0.255.256",
      "127.0.0.255.255.255",
    ];
    for case in pass_cases {
      assert!(is_ipv4(case));
    }
    for case in fail_cases {
      assert!(!is_ipv4(case));
    }
  }

  #[test]
  fn test_is_ipv6() {
    let pass_cases = [
      "::1",
      "::ffff:127.0.0.1",
      "::ffff:192.168.1.1",
      "::ffff:10.0.0.1",
      "::ffff:172.16.0.1",
      "::ffff:172.31.255.255",
      "::ffff:100.64.0.1",
      "::ffff:100.127.255.255",
    ];
    let fail_cases = [
      "::1.1",
      "::ffff:127.0.0.1.1",
      "::ffff:127.0.0.0.1",
      "::ffff:127.0.0.256",
      "::ffff:127.0.0.-1",
      "::ffff:127.0.0.255.256",
      "::ffff:127.0.0.255.255.255",
    ];
    for case in pass_cases {
      assert!(is_ipv6(case));
    }
    for case in fail_cases {
      assert!(!is_ipv6(case));
    }
  }

  #[test]
  fn test_is_ip() {
    let ipv4_cases = [
      "127.0.0.1",
      "192.168.1.1",
      "10.0.0.1",
      "172.16.0.1",
      "172.31.255.255",
      "100.64.0.1",
      "100.127.255.255",
      "169.254.0.1",
      "169.254.255.255",
    ];
    let ipv6_cases = [
      "::1",
      "::ffff:127.0.0.1",
      "::ffff:192.168.1.1",
      "::ffff:10.0.0.1",
      "::ffff:172.16.0.1",
      "::ffff:172.31.255.255",
      "::ffff:100.64.0.1",
      "::ffff:100.127.255.255",
      "::ffff:169.254.0.1",
      "::ffff:169.254.255.255",
    ];
    let fail_cases = [
      "127.0.0.1.1",
      "127.0.0.0.1",
      "127.0.0.256",
      "127.0.0.-1",
      "127.0.0.255.256",
      "127.0.0.255.255.255",
    ];
    for case in ipv4_cases {
      assert_eq!(is_ip(case), 4);
    }
    for case in ipv6_cases {
      assert_eq!(is_ip(case), 6);
    }
    for case in fail_cases {
      assert_eq!(is_ip(case), 0);
    }
  }
}
