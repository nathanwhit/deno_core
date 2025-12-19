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
use std::cell::RefCell;
use std::ops::DerefMut;
use std::rc::Rc;
use std::str::FromStr;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;

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

struct SocketCbInner {
  write: Rc<AsyncRefCell<Option<tokio::net::tcp::OwnedWriteHalf>>>,
  cb: RefCell<Option<Rc<v8::TracedReference<v8::Function>>>>,
  context: Rc<v8::Global<v8::Context>>,
  host: String,
  port: u16,

  ops_tracker: ExternalOpsTracker,
  cancel: Rc<CancelHandle>,
}

pub struct SocketCb {
  inner: Rc<SocketCbInner>,
}

unsafe impl deno_core::GarbageCollected for SocketCb {
  fn trace(&self, visitor: &mut v8::cppgc::Visitor) {
    if let Some(cb) = self.inner.cb.borrow().as_ref() {
      cb.trace(visitor);
    }
  }

  fn get_name(&self) -> &'static std::ffi::CStr {
    c"SocketCb"
  }
}

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
  pub fn new(
    scope: &mut v8::PinScope,
    op_state: &mut OpState,
    #[string] host: String,
    #[smi] port: u16,
  ) -> Result<SocketCb, JsErrorBox> {
    let context = v8::Global::new(scope, scope.get_current_context());
    let ops_tracker = op_state.external_ops_tracker.clone();
    let cb = SocketCb {
      inner: Rc::new(SocketCbInner {
        write: Rc::new(AsyncRefCell::new(None)),
        cb: RefCell::new(None),
        context: Rc::new(context),
        cancel: Rc::new(CancelHandle::new()),
        host,
        port,
        ops_tracker,
      }),
    };
    Ok(cb)
  }

  #[async_method]
  pub fn connect<'a, 'b>(
    &self,
    #[this] this: v8::Global<v8::Object>,
    scope: v8::UnsafeRawIsolatePtr,
    #[global] cb: v8::Global<v8::Function>,
  ) -> impl Future<Output = Result<(), JsErrorBox>> {
    let scope_holder = ScopeHolder::new(scope, self.inner.context.clone());
    let inner = self.inner.clone();
    let this = Rc::new(this);
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
    inner.ops_tracker.ref_op();
    async move {
      let stream =
        tokio::net::TcpStream::connect((inner.host.as_str(), inner.port))
          .await
          .map_err(JsErrorBox::from_err)
          .unwrap();
      let (read, write) = stream.into_split();
      *inner.write.borrow_mut().await = Some(write);
      let ops_tracker = inner.ops_tracker.clone();
      deno_core::unsync::spawn(async move {
        let cb = inner.cb.borrow().as_ref().unwrap().clone();
        let this = this.clone();
        let mut buf_reader = tokio::io::BufReader::new(read);
        while let Ok(buf) = buf_reader.fill_buf().await {
          // eprintln!("got data");
          if buf.is_empty() {
            // eprintln!("empty buf!");
            break;
          }
          let nread = buf.len();
          let data = Uint8Array::from(buf.to_vec());

          buf_reader.consume(nread);
          // eprintln!("CALLING CB");
          {
            let mut raw_isolate = unsafe {
              v8::Isolate::from_raw_isolate_ptr_unchecked(scope_holder.0)
            };
            v8::scope!(let scope, &mut raw_isolate);
            let context = v8::Local::new(scope, &*scope_holder.1);
            let scope = &mut v8::ContextScope::new(scope, context);
            let this = v8::Local::new(scope, &*this);
            let cb = cb.get(scope).unwrap();
            let Ok(data_v8) = data.to_v8(scope);
            let _result = cb.call(scope, this.into(), &[data_v8]).unwrap();
          };
        }
        // eprintln!("Disconnected");
      });

      // ops_tracker.unref_op();
      Ok(())
    }
  }

  #[fast]
  fn unref(&self) {
    self.inner.ops_tracker.unref_op();
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
