//! Background jobs and timers.
//!
//! The editor is single-threaded. Slow work (processes, searches, language servers) runs
//! on a job thread and sends closures back with `JobContext::send`; they run on the UI
//! thread during `Editor::poll`, with full access to the editor. A frontend-supplied waker
//! lets the UI sleep until something arrives.

use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::editor::Editor;

type UiTask = Box<dyn FnOnce(&mut Editor) + Send>;
type TimerFn = Rc<dyn Fn(&mut Editor) -> bool>;
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// Cancels a job: its pending and future results are dropped, and the job can observe
/// `JobContext::is_cancelled` to stop early.
#[derive(Clone, Debug, Default)]
pub struct JobHandle {
    cancelled: Arc<AtomicBool>,
}

impl JobHandle {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// Given to a job thread to report back to the UI thread. Clone it to report from several
/// threads (e.g. a library's own event loop).
#[derive(Clone)]
pub struct JobContext {
    handle: JobHandle,
    tx: Sender<(JobHandle, UiTask)>,
    waker: Arc<OnceLock<Waker>>,
}

impl JobContext {
    pub fn is_cancelled(&self) -> bool {
        self.handle.is_cancelled()
    }

    /// Runs `f` on the UI thread. Returns false once the job is cancelled or the editor
    /// has shut down, so loops can stop producing.
    pub fn send(&self, f: impl FnOnce(&mut Editor) + Send + 'static) -> bool {
        if self.is_cancelled() || self.tx.send((self.handle.clone(), Box::new(f))).is_err() {
            return false;
        }
        if let Some(wake) = self.waker.get() {
            wake();
        }
        true
    }
}

struct Timer {
    interval: Duration,
    due: Instant,
    /// Returns whether it changed anything visible.
    run: TimerFn,
}

pub struct Scheduler {
    tx: Sender<(JobHandle, UiTask)>,
    rx: Receiver<(JobHandle, UiTask)>,
    waker: Arc<OnceLock<Waker>>,
    timers: Vec<Timer>,
}

impl Default for Scheduler {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        Self { tx, rx, waker: Arc::new(OnceLock::new()), timers: Vec::new() }
    }
}

impl Scheduler {
    pub(crate) fn set_waker(&self, waker: Waker) {
        let _ = self.waker.set(waker);
    }

    pub(crate) fn spawn(&self, job: impl FnOnce(JobContext) + Send + 'static) -> JobHandle {
        let (handle, ctx) = self.context();
        std::thread::spawn(move || job(ctx));
        handle
    }

    pub(crate) fn context(&self) -> (JobHandle, JobContext) {
        let handle = JobHandle::default();
        let ctx = JobContext { handle: handle.clone(), tx: self.tx.clone(), waker: self.waker.clone() };
        (handle, ctx)
    }

    pub(crate) fn add_timer(&mut self, interval: Duration, run: impl Fn(&mut Editor) -> bool + 'static) {
        self.timers.push(Timer { interval, due: Instant::now() + interval, run: Rc::new(run) });
    }

    /// Results that arrived from jobs that are still wanted.
    pub(crate) fn take_results(&self) -> Vec<UiTask> {
        self.rx.try_iter().filter(|(job, _)| !job.is_cancelled()).map(|(_, task)| task).collect()
    }

    /// Timers due at `now`, rescheduled for their next interval.
    pub(crate) fn take_due_timers(&mut self, now: Instant) -> Vec<TimerFn> {
        let mut due = Vec::new();
        for timer in &mut self.timers {
            if timer.due <= now {
                timer.due = now + timer.interval;
                due.push(timer.run.clone());
            }
        }
        due
    }

    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.timers.iter().map(|t| t.due).min()
    }
}
