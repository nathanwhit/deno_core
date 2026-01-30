// Copyright 2018-2025 the Deno authors. MIT license.
use std::{
  collections::VecDeque,
  pin::Pin,
  sync::{Arc, Mutex},
  task::{Context, Poll, Waker},
};

use bytes::Bytes;
use deno_error::JsErrorBox;
use http_body::Body;
use http_body::Frame;
use http_body::SizeHint;
use http_body_util::Either;
use http_body_util::Full;
use tokio::sync::Notify;

/// High water mark for response body backpressure.
pub const RESPONSE_BODY_HIGH_WATER: usize = 64 * 1024;

/// Internal state for response body streaming.
pub struct ResponseBodyState {
  queue: VecDeque<Bytes>,
  closed: bool,
  pending_bytes: usize,
  waker: Option<Waker>,
}

/// Handle for pushing data to the response body.
#[derive(Clone)]
pub struct ResponseBodyHandle {
  inner: Arc<Mutex<ResponseBodyState>>,
  drain_notify: Arc<Notify>,
}

/// Response body that implements hyper's Body trait.
pub struct ResponseBody {
  inner: Arc<Mutex<ResponseBodyState>>,
  drain_notify: Arc<Notify>,
}

/// Type alias for HTTP response body (either full or streaming).
pub type HttpResponseBody = Either<Full<Bytes>, ResponseBody>;

/// Create a paired handle and body for response streaming.
pub fn response_body_pair() -> (ResponseBodyHandle, ResponseBody) {
  let inner = Arc::new(Mutex::new(ResponseBodyState {
    queue: VecDeque::new(),
    closed: false,
    pending_bytes: 0,
    waker: None,
  }));
  let drain_notify = Arc::new(Notify::new());
  (
    ResponseBodyHandle {
      inner: inner.clone(),
      drain_notify: drain_notify.clone(),
    },
    ResponseBody {
      inner,
      drain_notify,
    },
  )
}

impl ResponseBodyHandle {
  /// Push bytes to the response body. Returns the current pending byte count.
  pub fn push_bytes(&self, bytes: Bytes) -> Result<usize, JsErrorBox> {
    if bytes.is_empty() {
      return Ok(0);
    }
    let (pending, waker) = {
      let mut state = self.inner.lock().unwrap();
      if state.closed {
        return Err(JsErrorBox::generic("Response body closed"));
      }
      state.pending_bytes = state.pending_bytes.saturating_add(bytes.len());
      state.queue.push_back(bytes);
      (state.pending_bytes, state.waker.take())
    };
    if let Some(waker) = waker {
      waker.wake();
    }
    Ok(pending)
  }

  /// Close the response body.
  pub fn close(&self) {
    let waker = {
      let mut state = self.inner.lock().unwrap();
      state.closed = true;
      state.waker.take()
    };
    if let Some(waker) = waker {
      waker.wake();
    }
    self.drain_notify.notify_waiters();
  }

  /// Wait for the pending bytes to drain below the target.
  pub async fn wait_for_drain(&self, target: usize) {
    loop {
      let pending = {
        let state = self.inner.lock().unwrap();
        if state.closed || state.pending_bytes <= target {
          return;
        }
        state.pending_bytes
      };
      let _ = pending;
      self.drain_notify.notified().await;
    }
  }
}

impl Body for ResponseBody {
  type Data = Bytes;
  type Error = hyper::Error;

  fn poll_frame(
    self: Pin<&mut Self>,
    cx: &mut Context<'_>,
  ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
    let (frame, closed, pending_bytes) = {
      let mut state = self.inner.lock().unwrap();
      if let Some(bytes) = state.queue.pop_front() {
        state.pending_bytes = state.pending_bytes.saturating_sub(bytes.len());
        let pending_bytes = state.pending_bytes;
        let frame = Frame::data(bytes);
        (Some(Ok(frame)), state.closed, pending_bytes)
      } else if state.closed {
        (None, true, state.pending_bytes)
      } else {
        state.waker = Some(cx.waker().clone());
        return Poll::Pending;
      }
    };

    if pending_bytes <= RESPONSE_BODY_HIGH_WATER {
      self.drain_notify.notify_waiters();
    }

    if closed && frame.is_none() {
      return Poll::Ready(None);
    }
    Poll::Ready(frame)
  }

  fn is_end_stream(&self) -> bool {
    let state = self.inner.lock().unwrap();
    state.closed && state.queue.is_empty()
  }

  fn size_hint(&self) -> SizeHint {
    SizeHint::new()
  }
}
