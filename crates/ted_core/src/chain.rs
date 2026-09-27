//! Backend chains: a feature asks its backends in priority order until one answers.
//!
//! Cross-references, completion and formatting don't know where answers come from. A
//! feature defines a `Request` (what was asked, and what to do with the answer) and
//! backends register for it: a language server ahead of a built-in fallback such as
//! tree-sitter tags or the words in open buffers. A backend that declines, fails or finds
//! nothing hands the request to the next one.
//!
//! Backends answer asynchronously through the `Reply` they are handed. It is plain data,
//! so it can travel through a job thread and back. Each feature has at most one request
//! pending: asking again drops the older one, and an answer the user has moved on from
//! (`Request::is_current`) is dropped too.

use std::marker::PhantomData;
use std::rc::Rc;

use crate::editor::Editor;

pub trait Request: Clone + 'static {
    type Answer: 'static;

    /// Whether an answer arriving now is still wanted.
    fn is_current(&self, ed: &Editor) -> bool;

    /// Whether `answer` counts as nothing found, passing the request to the next backend.
    fn is_empty(answer: &Self::Answer) -> bool;

    /// Uses the first answer that isn't empty.
    fn answer(self, ed: &mut Editor, answer: Self::Answer);

    /// Every backend declined, failed or found nothing; `errors` holds the failures.
    fn unanswered(self, ed: &mut Editor, errors: Vec<String>);

    /// The request ended without an answer or a failure: the user moved on, or a newer
    /// request of the same kind replaced it.
    fn dropped(self, _ed: &mut Editor) {}
}

pub trait Backend<R: Request>: 'static {
    fn name(&self) -> &str;

    /// Starts answering `request`, eventually calling `reply.send`. Returns false (without
    /// using `reply`) to decline, e.g. when no server handles the buffer.
    fn start(&self, ed: &mut Editor, request: &R, reply: Reply<R>) -> bool;
}

/// The way back to the request a backend is answering.
#[must_use = "a request stays pending until its reply is sent"]
pub struct Reply<R> {
    serial: u64,
    /// Index of the backend to try next if this one comes up empty.
    next: usize,
    _request: PhantomData<fn() -> R>,
}

impl<R> std::fmt::Debug for Reply<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Reply").field("serial", &self.serial).field("next", &self.next).finish()
    }
}

impl<R: Request> Reply<R> {
    /// Delivers a backend's answer. An error or an empty answer passes the request on to
    /// the next backend.
    pub fn send(self, ed: &mut Editor, result: Result<R::Answer, String>) {
        let pending = ed.ext::<Chain<R>>().and_then(|c| c.pending.as_ref()).filter(|p| p.serial == self.serial);
        let Some(current) = pending.map(|p| p.request.is_current(ed)) else {
            return;
        };
        if !current {
            if let Some(pending) = take::<R>(ed, self.serial) {
                pending.request.dropped(ed);
            }
            return;
        }
        match result {
            Ok(answer) if !R::is_empty(&answer) => {
                if let Some(pending) = take::<R>(ed, self.serial) {
                    pending.request.answer(ed, answer);
                }
            }
            Ok(_) => try_backends::<R>(ed, self.serial, self.next),
            Err(e) => {
                if let Some(pending) = &mut ed.ext_mut::<Chain<R>>().pending {
                    pending.errors.push(e);
                }
                try_backends::<R>(ed, self.serial, self.next);
            }
        }
    }
}

struct Pending<R> {
    serial: u64,
    request: R,
    errors: Vec<String>,
}

struct Chain<R> {
    /// Highest priority first.
    backends: Vec<(i32, Rc<dyn Backend<R>>)>,
    pending: Option<Pending<R>>,
    serial: u64,
}

impl<R> Default for Chain<R> {
    fn default() -> Self {
        Self { backends: Vec::new(), pending: None, serial: 0 }
    }
}

/// Adds a backend. Higher `priority` is asked first; built-in fallbacks sit at 0 and
/// language servers at 100.
pub fn register<R: Request>(ed: &mut Editor, priority: i32, backend: impl Backend<R>) {
    let backends = &mut ed.ext_mut::<Chain<R>>().backends;
    let at = backends.partition_point(|(p, _)| *p >= priority);
    backends.insert(at, (priority, Rc::new(backend)));
}

/// Asks the backends of `R`, dropping the request of that kind still pending.
pub fn ask<R: Request>(ed: &mut Editor, request: R) {
    let chain = ed.ext_mut::<Chain<R>>();
    chain.serial += 1;
    let serial = chain.serial;
    let older = chain.pending.replace(Pending { serial, request, errors: Vec::new() });
    if let Some(older) = older {
        older.request.dropped(ed);
    }
    try_backends::<R>(ed, serial, 0);
}

/// The request of kind `R` still waiting for an answer.
pub fn pending<R: Request>(ed: &Editor) -> Option<&R> {
    Some(&ed.ext::<Chain<R>>()?.pending.as_ref()?.request)
}

/// Hands pending request `serial` to the first backend from index `from` that accepts it;
/// reports it unanswered when none is left.
fn try_backends<R: Request>(ed: &mut Editor, serial: u64, from: usize) {
    let chain = ed.ext_mut::<Chain<R>>();
    let Some(request) = chain.pending.as_ref().filter(|p| p.serial == serial).map(|p| p.request.clone()) else {
        return;
    };
    let backends: Vec<_> = chain.backends.iter().skip(from).map(|(_, b)| b.clone()).collect();
    for (i, backend) in backends.into_iter().enumerate() {
        let reply = Reply { serial, next: from + i + 1, _request: PhantomData };
        if backend.start(ed, &request, reply) {
            return;
        }
    }
    if let Some(pending) = take::<R>(ed, serial) {
        pending.request.unanswered(ed, pending.errors);
    }
}

/// Removes pending request `serial`, if it is still the one pending.
fn take<R: Request>(ed: &mut Editor, serial: u64) -> Option<Pending<R>> {
    let pending = &mut ed.ext_mut::<Chain<R>>().pending;
    pending.take_if(|p| p.serial == serial)
}
