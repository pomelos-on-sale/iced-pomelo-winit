//! The executor: where a `Program`'s futures run.
//!
//! iced asks the platform for an [`Executor`], and on a desktop that means a thread pool, tokio or
//! smol — something that can run a future *beside* the event loop and wake it from a timer or
//! another thread. None of that exists here. The loop is one task on the board's runtime and every
//! frame is a poll, so the executor is a queue that [`Pump::tick`] drains, and what iced's own
//! runtimes get from a waker (a wakeup arriving from elsewhere, a timer firing while the loop
//! sleeps) is answered here by ticking often enough.
//!
//! What that buys, and what it costs:
//!
//! * a future runs *between* frames, never during one: a slow future costs a frame, and it cannot
//!   preempt the widget tree or race it, because nothing polls from another thread;
//! * a future that never yields blocks the frames that would have driven it, so the contract for
//!   whoever writes one is "short, and pollable";
//! * there is no runtime to carry: no tokio, no async-std, no threads, nothing that has to build
//!   for ESP-IDF.
//!
//! `iced_futures`' own `null` backend is what a `Program` gets if it names the platform default,
//! and it drops every future instead of running it — which is why the host builds this one and
//! does not ask the app for its executor. See `crate::program`.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};

use iced_futures::{Executor, MaybeSend};

/// Tasks waiting for the next [`Pump::tick`].
#[derive(Default)]
struct Queue {
    futures: Mutex<Vec<Pin<Box<dyn Future<Output = ()> + Send>>>>,
    woken: AtomicBool,
}

/// The executor the host runs a `Program` on.
///
/// Cloning one shares its queue, which is how the caller keeps a handle: `iced_futures::Runtime`
/// owns the executor it was given, and there is no way to ask it for it back.
#[derive(Clone, Default)]
pub struct Pump(Arc<Queue>);

impl Pump {
    /// An empty executor.
    pub fn new() -> Self {
        Self::default()
    }

    /// Polls every task once, which is what "running" means here.
    ///
    /// Called once per frame, before the frame's messages are applied: a task that finished during
    /// the last frame delivers its message in this one, in the same frame that draws the result.
    pub fn tick(&self) {
        let waker = Waker::from(Arc::new(Signal(Arc::clone(&self.0))));
        let mut context = Context::from_waker(&waker);

        let mut waiting = Vec::new();

        {
            let mut queue = self.0.futures.lock().expect("the executor's queue");
            let running = std::mem::take(&mut *queue);

            for mut future in running {
                match future.as_mut().poll(&mut context) {
                    Poll::Ready(()) => {}
                    Poll::Pending => waiting.push(future),
                }
            }

            *queue = waiting;
        }

        self.0.woken.store(false, Ordering::Relaxed);
    }

    /// How many tasks are waiting for a poll.
    ///
    /// For the frame log: a Program's work is invisible in the damage numbers, and this is the
    /// cheapest way to see whether the frames a subscription is asking for are still being asked
    /// for.
    pub fn pending(&self) -> usize {
        self.0.futures.lock().expect("the executor's queue").len()
    }
}

impl Executor for Pump {
    fn new() -> Result<Self, iced_futures::futures::io::Error> {
        Ok(Self::default())
    }

    /// Queues `future`. It runs at the next [`Pump::tick`], on the frame's thread.
    fn spawn(&self, future: impl Future<Output = ()> + MaybeSend + 'static) {
        self.0
            .futures
            .lock()
            .expect("the executor's queue")
            .push(Box::pin(future));
    }

    /// Polls `future` until it is ready.
    ///
    /// Nothing in the host calls this, and nothing should: there is no runtime to block, so a
    /// future that is waiting for anything other than itself can never finish here, and returning
    /// a value that will never arrive is worse than saying so. The trait asks for the method; this
    /// is the honest implementation of it on a platform that cannot.
    fn block_on<T>(&self, future: impl Future<Output = T>) -> T {
        let waker = Waker::from(Arc::new(Signal(Arc::clone(&self.0))));
        let mut context = Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);

        // One poll, because there is nothing here that could make a second one different: a frame
        // is the only thing that advances a future, and a frame cannot happen while this call is
        // on the stack.
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!(
                "`Executor::block_on` cannot wait on this platform: the only thing that advances \
                 a future here is a frame, and a frame cannot happen while this call is on the \
                 stack"
            ),
        }
    }
}

/// The waker: it records that something wants to be polled again.
///
/// Nothing reads it — [`Pump::tick`] polls every task whether it was woken or not, so a lost
/// wakeup costs a poll rather than a hang. It exists so that a future which insists on a real
/// waker (most of them do) gets one, and so that the "poll only what was woken" optimisation has
/// the flag it needs when someone measures whether it is worth doing.
///
/// If that optimisation ever lands, this is the line that has to be believed: everything below
/// assumes that a missed wakeup is survivable *because* every task is polled anyway.
struct Signal(Arc<Queue>);

impl Wake for Signal {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.woken.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    /// A future that is ready on its own `n`th poll.
    struct Later(u8);

    impl Future for Later {
        type Output = u8;

        fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<u8> {
            self.0 -= 1;

            if self.0 == 0 {
                Poll::Ready(self.0)
            } else {
                context.waker().wake_by_ref();
                Poll::Pending
            }
        }
    }

    #[test]
    fn a_task_runs_on_the_next_tick_and_not_before() {
        let pump = Pump::new();
        let polls = Arc::new(AtomicU32::new(0));

        let counted = Arc::clone(&polls);
        pump.spawn(async move {
            counted.fetch_add(1, Ordering::Relaxed);
        });

        assert_eq!(polls.load(Ordering::Relaxed), 0, "spawning is not running");

        pump.tick();

        assert_eq!(polls.load(Ordering::Relaxed), 1);
        assert_eq!(pump.pending(), 0, "a finished task is dropped");
    }

    #[test]
    fn a_task_that_is_not_ready_is_polled_again() {
        let pump = Pump::new();
        let finished = Arc::new(AtomicU32::new(0));

        let counted = Arc::clone(&finished);
        pump.spawn(async move {
            Later(3).await;
            counted.fetch_add(1, Ordering::Relaxed);
        });

        pump.tick();
        assert_eq!(finished.load(Ordering::Relaxed), 0);
        assert_eq!(pump.pending(), 1);

        pump.tick();
        assert_eq!(finished.load(Ordering::Relaxed), 0);

        pump.tick();
        assert_eq!(finished.load(Ordering::Relaxed), 1);
        assert_eq!(pump.pending(), 0);
    }
}
