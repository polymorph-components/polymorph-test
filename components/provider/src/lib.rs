//! Reference context provider.
//!
//! Semantics:
//! - Diagnostics are black-holed until `observer.diagnostics()` is called.
//! - After that, `context.diagnostic` awaits delivery into the stream
//!   (cooperative backpressure; the runner core must drain concurrently).
//! - Dropping the context closes the stream (reader sees end-of-stream).
//! - `observer.diagnostics()` traps on a second call.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: "wit",
        world: "provider",
        generate_all,
    });
}

use std::cell::RefCell;
use std::rc::Rc;

use bindings::exports::polymorph::component_test::test_context::{
    Guest as ContextIfaceGuest, GuestContext,
};
use bindings::exports::polymorph::component_test_provider::factory::{
    Context as ContextHandle, Guest as FactoryGuest, GuestObserver, Observer as ObserverHandle,
};
use bindings::wit_stream;
use wit_bindgen::rt::async_support::StreamWriter;

/// State shared between the two ends of a pair.
#[derive(Default)]
struct Shared {
    /// Some(writer) once observed and open; None before observation
    /// (black hole) and after close/hangup.
    writer: Option<StreamWriter<String>>,
    observed: bool,
    /// Set by `Context::drop`: the stream is closed for good. Without
    /// this, a write completing after the drop couldn't tell "writer
    /// absent because I took it" from "absent because the context was
    /// dropped", and would resurrect a closed stream (wedging a
    /// post-run drain that waits for end-of-stream).
    closed: bool,
}

struct Context {
    shared: Rc<RefCell<Shared>>,
}

impl Drop for Context {
    fn drop(&mut self) {
        // Close the stream: reader observes end-of-stream.
        let mut shared = self.shared.borrow_mut();
        shared.writer = None;
        shared.closed = true;
    }
}

impl GuestContext for Context {
    async fn diagnostic(&self, msg: String) {
        // Take the writer out for the duration of the await so a
        // concurrent call can't alias it (it black-holes instead).
        let writer = self.shared.borrow_mut().writer.take();
        if let Some(mut writer) = writer {
            let rejected = writer.write_one(msg).await;
            if rejected.is_none() {
                // Delivered; put the writer back unless the context was
                // dropped (closed) or hung up meanwhile.
                let mut shared = self.shared.borrow_mut();
                if shared.observed && !shared.closed && shared.writer.is_none() {
                    shared.writer = Some(writer);
                }
            }
            // rejected = Some(_) means the reader hung up: writer is
            // dropped here and further diagnostics are black-holed.
        }
    }
}

struct Observer {
    shared: Rc<RefCell<Shared>>,
}

impl GuestObserver for Observer {
    fn diagnostics(&self) -> wit_bindgen::rt::async_support::StreamReader<String> {
        let mut shared = self.shared.borrow_mut();
        assert!(!shared.observed, "observer.diagnostics called twice");
        shared.observed = true;
        let (tx, rx) = wit_stream::new::<String>();
        shared.writer = Some(tx);
        rx
    }
}

struct Provider;

impl ContextIfaceGuest for Provider {
    type Context = Context;
}

impl FactoryGuest for Provider {
    type Observer = Observer;

    fn new_context() -> (ContextHandle, ObserverHandle) {
        let shared = Rc::new(RefCell::new(Shared::default()));
        (
            ContextHandle::new(Context {
                shared: shared.clone(),
            }),
            ObserverHandle::new(Observer { shared }),
        )
    }
}

bindings::export!(Provider with_types_in bindings);
