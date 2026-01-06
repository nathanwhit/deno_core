use deno_core::AsyncRefCell;
use deno_core::CancelFuture;
use deno_core::CancelHandle;
use deno_core::ExternalOpsTracker;
use deno_core::JsBuffer;
use deno_core::OpState;
use deno_core::ToV8;
use deno_core::convert::Uint8Array;
use deno_core::op2;
use deno_core::v8;
use deno_core::v8::cppgc::Traced;
use deno_error::JsErrorBox;
use std::cell::RefCell;
use std::ops::DerefMut;
use std::rc::Rc;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

const READ_BUFFER_SIZE: usize = 64 * 1024;

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

struct ReadSignal {
  pending: AtomicBool,
  notify: tokio::sync::Notify,
}
impl ReadSignal {
  fn new() -> Self {
    Self {
      pending: AtomicBool::new(false),
      notify: tokio::sync::Notify::new(),
    }
  }

  fn signal(&self) {
    if !self.pending.swap(true, Ordering::Relaxed) {
      self.notify.notify_waiters();
    }
  }

  fn clear(&self) {
    self.pending.store(false, Ordering::Relaxed);
  }

  async fn wait(&self) {
    if self.pending.load(Ordering::Relaxed) {
      return;
    }
    self.notify.notified().await;
  }
}

struct Connected {
  connected: AtomicBool,
  notify: tokio::sync::Notify,
}
impl Connected {
  fn new() -> Self {
    Self {
      connected: AtomicBool::new(false),
      notify: tokio::sync::Notify::new(),
    }
  }

  fn signal(&self) {
    self.connected.store(true, Ordering::Relaxed);
    self.notify.notify_waiters();
  }

  async fn wait(&self) {
    if self.connected.load(Ordering::Relaxed) {
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
    }
  }
}

struct SocketCbInner {
  write: Rc<AsyncRefCell<Option<tokio::net::tcp::OwnedWriteHalf>>>,
  read: Rc<AsyncRefCell<Option<tokio::net::tcp::OwnedReadHalf>>>,
  push_func: Rc<v8::TracedReference<v8::Function>>,
  host: String,
  port: u16,
  this: Rc<v8::TracedReference<v8::Object>>,

  ref_tracker: RefTracker,
  cancel: Rc<CancelHandle>,
  connected: Rc<Connected>,
  scope_holder: ScopeHolder,

  read_signal: Rc<ReadSignal>,
  read_started: AtomicBool,
}

pub struct SocketCb {
  inner: Rc<SocketCbInner>,
}

unsafe impl deno_core::GarbageCollected for SocketCb {
  fn trace(&self, visitor: &mut v8::cppgc::Visitor) {
    self.inner.push_func.trace(visitor);
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

  pub fn with_scope<R>(&self, f: impl FnOnce(&mut v8::PinScope) -> R) -> R {
    let mut raw_isolate =
      unsafe { v8::Isolate::from_raw_isolate_ptr_unchecked(self.0) };
    v8::scope!(let scope, &mut raw_isolate);
    let context = v8::Local::new(scope, &*self.1);
    let scope = &mut v8::ContextScope::new(scope, context);
    f(scope)
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
    let push = internalized(scope, "push");
    let push_func = local_me
      .get(scope, push.into())
      .unwrap()
      .cast::<v8::Function>();
    let push_func = Rc::new(v8::TracedReference::new(scope, push_func));
    let context = Rc::new(context);
    let scope = unsafe { scope.as_raw_isolate_ptr() };
    let scope_holder = ScopeHolder::new(scope, context);

    let cb = SocketCb {
      inner: Rc::new(SocketCbInner {
        write: Rc::new(AsyncRefCell::new(None)),
        read: Rc::new(AsyncRefCell::new(None)),
        push_func,
        cancel: Rc::new(CancelHandle::new()),
        host,
        port,
        this,
        ref_tracker: RefTracker::new(ops_tracker),
        connected: Rc::new(Connected::new()),
        scope_holder,
        read_signal: Rc::new(ReadSignal::new()),
        read_started: AtomicBool::new(false),
      }),
    };
    Ok(cb)
  }

  #[async_method]
  pub fn connect<'a, 'b>(
    &self,
  ) -> impl Future<Output = Result<(), JsErrorBox>> {
    let inner = self.inner.clone();
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
      inner.connected.signal();
      Ok(())
    }
  }

  #[fast]
  #[rename("_read")]
  fn read(&self, #[this] me: v8::Global<v8::Object>) {
    self.inner.read_signal.signal();
    if self.inner.read_started.swap(true, Ordering::Relaxed) {
      return;
    }

    self.start_read(Rc::new(me));
  }

  #[fast]
  #[rename("ref")]
  fn r#ref(&self) {
    self.inner.ref_tracker.ref_();
  }

  #[fast]
  fn unref(&self) {
    self.inner.ref_tracker.unref();
  }

  #[rename("_final")]
  fn final_(&self, #[global] cb: v8::Global<v8::Value>) {
    let inner = self.inner.clone();
    deno_core::unsync::spawn(async move {
      let mut write = inner.write.borrow_mut().await;
      let write = write.deref_mut().as_mut().unwrap();
      let result = write.shutdown().await.map_err(JsErrorBox::from_err).err();
      call_write_cb(&inner, &cb, result);
    });
  }

  #[reentrant]
  #[rename("_destroy")]
  fn destroy(
    &self,
    #[global] error: v8::Global<v8::Value>,
    #[global] cb: v8::Global<v8::Function>,
  ) {
    self.inner.cancel.cancel();
    self.inner.read_signal.clear();
    let inner = self.inner.clone();

    deno_core::unsync::spawn(async move {
      inner.read.borrow_mut().await.take();
      inner.write.borrow_mut().await.take();
      inner.scope_holder.with_scope(|scope| {
        let cb = v8::Local::new(scope, &cb);
        let this = inner.this.get(scope).unwrap();
        let error = v8::Local::new(scope, &error);
        cb.call(scope, this.into(), &[error]).unwrap();
      });
      inner.ref_tracker.unref();
    });
  }

  #[reentrant]
  #[rename("_write")]
  pub fn write(
    &self,
    #[buffer] data: JsBuffer,
    _encoding: v8::Local<v8::String>,
    #[global] cb: v8::Global<v8::Value>,
  ) -> Result<(), JsErrorBox> {
    let inner = self.inner.clone();
    let num_wrote = self.try_sync_write(&data)?;

    if num_wrote >= data.len() {
      call_write_cb(&inner, &cb, None);
      return Ok(());
    }

    deno_core::unsync::spawn(async move {
      let result = inner
        .write
        .borrow_mut()
        .await
        .deref_mut()
        .as_mut()
        .unwrap()
        .write_all(&data[num_wrote..])
        .or_cancel(inner.cancel.clone())
        .await
        .unwrap()
        .map_err(JsErrorBox::from_err)
        .err();

      call_write_cb(&inner, &cb, result);
    });

    Ok(())
  }
}

impl SocketCb {
  fn try_sync_write(&self, data: &JsBuffer) -> Result<usize, JsErrorBox> {
    let Some(mut write) = self.inner.write.try_borrow_mut() else {
      return Ok(0);
    };
    let write = write.deref_mut().as_mut().unwrap();

    match write.try_write(data) {
      Ok(n) => Ok(n),
      Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(0),
      Err(e) => {
        eprintln!("error writing: {:?}", e);
        Err(JsErrorBox::from_err(e))
      }
    }
  }

  fn start_read(&self, this: Rc<v8::Global<v8::Object>>) {
    let inner = self.inner.clone();
    deno_core::unsync::spawn(async move {
      inner.connected.wait().await;

      let mut read = inner.read.borrow_mut().await;
      let read = read.deref_mut().as_mut().unwrap();
      let mut buf = vec![0; READ_BUFFER_SIZE];

      loop {
        let nread = match inner.read_next(&mut buf, read).await {
          Ok(n) => n,
          Err(e) => {
            inner.emit_error(e);
            break;
          }
        };

        inner.handle_read_data(&this, &buf[..nread]);
      }
      inner.read_signal.clear();
    });
  }
}

fn call_write_cb(
  inner: &SocketCbInner,
  cb: &v8::Global<v8::Value>,
  result: Option<JsErrorBox>,
) {
  inner.with_this(|scope, this| {
    let Ok(cb) = v8::Local::new(scope, cb).try_cast::<v8::Function>() else {
      eprintln!("cb is not a function");
      return;
    };
    match result {
      Some(err) => {
        let arg = err.to_v8(scope).unwrap();
        let _ = cb.call(scope, this.into(), &[arg]);
      }
      None => {
        let _ = cb.call(scope, this.into(), &[]);
      }
    }
  });
}

impl SocketCbInner {
  async fn read_next(
    &self,
    buf: &mut [u8],
    read: &mut tokio::net::tcp::OwnedReadHalf,
  ) -> Result<usize, std::io::Error> {
    let _ = self.read_signal.wait().or_cancel(self.cancel.clone()).await;
    match read.read(buf).or_cancel(self.cancel.clone()).await {
      Ok(Ok(n)) => Ok(n),
      Ok(Err(e)) => Err(e),
      Err(_) => Err(std::io::Error::new(
        std::io::ErrorKind::Interrupted,
        "cancelled",
      )),
    }
  }

  fn handle_read_data(&self, this: &Rc<v8::Global<v8::Object>>, data: &[u8]) {
    if data.is_empty() {
      self.push_eof(this);
    } else {
      self.push_data(this, data);
    }
  }

  fn push_eof(&self, this: &Rc<v8::Global<v8::Object>>) {
    let should_continue = self.scope_holder.with_scope(|scope| {
      let this = v8::Local::new(scope, &**this);
      let result = self.push_func.get(scope).unwrap().call(
        scope,
        this.into(),
        &[v8::null(scope).into()],
      );
      result.map_or(false, |r| r.cast::<v8::Boolean>().is_true())
    });

    if !should_continue {
      self.read_signal.clear();
    }
  }

  fn push_data(&self, this: &Rc<v8::Global<v8::Object>>, data: &[u8]) {
    let should_continue = self.scope_holder.with_scope(|scope| {
      v8::tc_scope!(let scope, scope);
      let this = v8::Local::new(scope, &**this);
      let data = Uint8Array(data.to_vec()).to_v8(scope).unwrap();
      let result =
        self
          .push_func
          .get(scope)
          .unwrap()
          .call(scope, this.into(), &[data]);
      result.map_or(false, |r| r.cast::<v8::Boolean>().is_true())
    });

    if !should_continue {
      eprintln!("push returned false");
      self.read_signal.clear();
    }
  }

  fn emit_error(&self, e: std::io::Error) {
    eprintln!("error reading: {:?}", e);
    self.call_method("emit", |scope, _| {
      let error = JsErrorBox::from_err(e).to_v8(scope).unwrap();
      vec![internalized(scope, "error").into(), error.into()]
    });
  }

  fn with_this<R>(
    &self,
    f: impl for<'a> FnOnce(
      &mut v8::PinScope<'a, '_>,
      v8::Local<'a, v8::Object>,
    ) -> R,
  ) -> R {
    self.scope_holder.with_scope(|scope| {
      let this = self.this.get(scope).unwrap();
      f(scope, this)
    })
  }

  fn call_method(
    &self,
    name: &str,
    build_args: impl for<'a> FnOnce(
      &mut v8::PinScope<'a, '_>,
      v8::Local<'a, v8::Object>,
    ) -> Vec<v8::Local<'a, v8::Value>>,
  ) {
    self.with_this(|scope, this| {
      let key = internalized(scope, name);
      let func = this.get(scope, key.into()).unwrap().cast::<v8::Function>();
      let args = build_args(scope, this);
      let _ = func.call(scope, this.into(), &args);
    });
  }
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

#[cfg(test)]
mod tests {
  use std::{
    sync::{OnceLock, atomic::AtomicUsize},
    time::Duration,
  };

  use deno_core::{ModuleSpecifier, RequestedModuleType, RuntimeOptions};

  use super::*;

  #[test]
  fn test_is_ipv4() {
    let cases = [
      ("127.0.0.1", true),
      ("192.168.1.1", true),
      ("172.16.0.1", true),
      ("169.254.255.255", true),
      ("127.0.0.1.1", false),
      ("127.0.0.256", false),
      ("127.0.0.-1", false),
    ];
    for (addr, expected) in cases {
      assert_eq!(is_ipv4(addr), expected, "failed for {}", addr);
    }
  }

  #[test]
  fn test_is_ipv6() {
    let cases = [
      ("::1", true),
      ("::ffff:127.0.0.1", true),
      ("::ffff:172.16.0.1", true),
      ("::1.1", false),
      ("::ffff:127.0.0.256", false),
      ("::ffff:127.0.0.-1", false),
    ];
    for (addr, expected) in cases {
      assert_eq!(is_ipv6(addr), expected, "failed for {}", addr);
    }
  }

  #[test]
  fn test_is_ip() {
    let cases = [
      ("127.0.0.1", 4),
      ("192.168.1.1", 4),
      ("::1", 6),
      ("::ffff:127.0.0.1", 6),
      ("127.0.0.1.1", 0),
      ("127.0.0.256", 0),
      ("invalid", 0),
    ];
    for (addr, expected) in cases {
      assert_eq!(is_ip(addr), expected, "failed for {}", addr);
    }
  }

  static ID: OnceLock<AtomicUsize> = OnceLock::new();
  fn next_id() -> usize {
    ID.get_or_init(|| AtomicUsize::new(0))
      .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
  }

  async fn js_test(
    contents: &str,
  ) -> Result<v8::Global<v8::Value>, JsErrorBox> {
    let (mut runtime, _worker_host_side) =
      crate::checkin::runner::create_runtime_without_snapshot(
        false,
        None,
        vec![],
        RuntimeOptions::default(),
      );
    let specifier =
      ModuleSpecifier::parse(&format!("file:///test-{}.ts", next_id()))
        .unwrap();

    let id = runtime
      .load_main_es_module_from_code(&specifier, contents.to_string())
      .await
      .map_err(JsErrorBox::from_err)?;

    let module = runtime.mod_evaluate(id);

    tokio::time::timeout(
      Duration::from_secs(10),
      runtime.run_event_loop(Default::default()),
    )
    .await
    .unwrap()
    .map_err(JsErrorBox::from_err)?;
    let _ = tokio::time::timeout(Duration::from_secs(10), module)
      .await
      .unwrap()
      .map_err(JsErrorBox::from_err)?;
    let namespace = runtime
      .get_module_namespace_by_name(
        &specifier.to_string(),
        RequestedModuleType::None,
      )
      .unwrap();
    deno_core::scope!(scope, runtime);
    let namespace = v8::Local::new(scope, namespace);
    let namespace = namespace.cast::<v8::Value>();
    Ok(v8::Global::new(scope, namespace))
  }

  #[tokio::test]
  async fn constructor_accepts_host_and_port() {
    let result = js_test(
      "
      const { SocketCb } = Deno.core.ops; 
      new SocketCb('localhost', 8080);
    ",
    );
    assert!(result.await.is_ok());
  }

  #[tokio::test(flavor = "current_thread")]
  async fn socket_read() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::task::spawn(async move {
      let (mut stream, _addr) = listener.accept().await.unwrap();
      let data = "hello world".as_bytes().to_vec();
      stream
        .write_all(&data)
        .await
        .map_err(JsErrorBox::from_err)
        .unwrap();
    });
    let port = addr.port();
    let code = "
    import { SocketCb } from 'checkin:net';
    import { equal } from 'checkin:testing';
    const socket = new SocketCb('localhost', ${PORT});
    
    const expected = new Uint8Array([
      104, 101, 108, 108,
      111,  32, 119, 111,
      114, 108, 100
    ]);

    const prom = Promise.withResolvers();
    let data = new Uint8Array();
    socket.on('data', (chunk) => {
      data = new Uint8Array([...data, ...chunk]);
      if (!equal(data, expected)) {
        throw new Error('data is not equal to expected');
      }
      prom.resolve();
      socket.destroy();
    });

    await socket.connect();
    await prom.promise;
  "
    .replace("${PORT}", &port.to_string());
    let result = js_test(&code);
    result.await.unwrap();
  }
}
