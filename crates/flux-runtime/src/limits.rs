//! Execution limits: CPU time slices, wall-clock timeouts and memory.
//!
//! * **Memory** — `Lua::set_memory_limit` on the extension's own state.
//! * **CPU slice** — a VM hook errors out when a coroutine runs longer than
//!   `slice_ms` without yielding. [`Sliced`] re-arms the deadline every time the
//!   future is polled, so legitimate async code (which yields at every `await`) is unaffected.
//! * **Wall-clock** — `tokio::time::timeout` around each run (see `worker.rs`).
//!
//! **`pcall` cannot defeat the limits.** A VM hook can only raise an ordinary Lua error, which
//! `pcall`/`xpcall`/`coroutine.resume` would swallow. So once a slice is exceeded the guard is
//! *tripped*: the hook raises again at every tick until the next [`SliceGuard::arm`], and the
//! sandbox's `pcall`/`xpcall`/`coroutine.resume` re-raise whenever the guard is tripped
//! (see `prelude.lua`). The error therefore always unwinds to the Rust caller.
//!
//! What a hook can *not* interrupt is work inside a single C function (a pathological
//! `string.find` pattern on a huge subject). [`Heartbeat`] lets the supervisor notice a stuck
//! worker and replace it, and worker threads run at a lower scheduling priority, but the real
//! fix is process isolation (`docs/security.md`).

use std::{
    cell::Cell,
    future::Future,
    pin::Pin,
    rc::Rc,
    sync::{Arc, OnceLock, atomic::{AtomicU64, Ordering}},
    task::{Context, Poll},
    time::{Duration, Instant},
};

use mlua::{HookTriggers, Lua, VmState};

fn epoch() -> Instant {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

fn now_ms() -> u64 {
    epoch().elapsed().as_millis() as u64 + 1 // never 0 (0 = idle)
}

/// Shared "currently running Lua since" marker, readable from other threads.
#[derive(Clone, Default)]
pub struct Heartbeat(Arc<AtomicU64>);

impl Heartbeat {
    /// Mirrors a remote worker's busy time (reported by an Extension Host in its pong).
    pub fn set_busy_for(&self, busy: Option<Duration>) {
        self.0.store(busy.map_or(0, |d| now_ms().saturating_sub(d.as_millis() as u64).max(1)), Ordering::Relaxed);
    }

    /// How long the worker has been inside a single poll of Lua code, if it is.
    pub fn busy_for(&self) -> Option<Duration> {
        match self.0.load(Ordering::Relaxed) {
            0 => None,
            t => Some(Duration::from_millis(now_ms().saturating_sub(t))),
        }
    }
}

#[derive(Clone)]
pub struct SliceGuard {
    deadline: Rc<Cell<Instant>>,
    slice: Duration,
    heartbeat: Heartbeat,
    /// Set when a slice is exceeded; cleared only by `arm`.
    tripped: Rc<Cell<bool>>,
    /// Hook ticks (10 000 VM instructions each) used in the current slice.
    ticks: Rc<Cell<u64>>,
}

/// VM instructions between two hook calls.
const HOOK_EVERY: u32 = 10_000;
/// Instruction budget per millisecond of slice: a deterministic cap next to the wall-clock one.
const INSTRUCTIONS_PER_MS: u64 = 100_000;

impl SliceGuard {
    /// Installs the memory limit and the instruction hook on `lua`.
    pub fn install(lua: &Lua, memory_mb: usize, slice: Duration, heartbeat: Heartbeat, tripped: Rc<Cell<bool>>) -> mlua::Result<Self> {
        if memory_mb > 0 {
            lua.set_memory_limit(memory_mb * 1024 * 1024)?;
        }
        let deadline = Rc::new(Cell::new(Instant::now() + slice));
        let ticks = Rc::new(Cell::new(0u64));
        let max_ticks = (slice.as_millis() as u64 * INSTRUCTIONS_PER_MS / HOOK_EVERY as u64).max(1);
        let (d, t, trip) = (deadline.clone(), ticks.clone(), tripped.clone());
        let ms = slice.as_millis();
        lua.set_global_hook(HookTriggers::new().every_nth_instruction(HOOK_EVERY), move |_, _| {
            t.set(t.get() + 1);
            if trip.get() || Instant::now() > d.get() || t.get() > max_ticks {
                trip.set(true);
                Err(mlua::Error::runtime(format!("execution limit exceeded ({ms} ms of CPU without yielding)")))
            } else {
                Ok(VmState::Continue)
            }
        })?;
        Ok(Self { deadline, slice, heartbeat, tripped, ticks })
    }

    /// Starts a new slice: synchronous Lua work (and every poll of a [`Sliced`] future) gets a fresh budget.
    pub fn arm(&self) {
        self.deadline.set(Instant::now() + self.slice);
        self.ticks.set(0);
        self.tripped.set(false);
    }

    pub fn sliced<F: Future>(&self, fut: F) -> Sliced<F> {
        Sliced { fut: Box::pin(fut), guard: self.clone() }
    }
}

pub struct Sliced<F: Future> {
    fut: Pin<Box<F>>,
    guard: SliceGuard,
}

impl<F: Future> Future for Sliced<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.guard.arm();
        self.guard.heartbeat.0.store(now_ms(), Ordering::Relaxed);
        let r = self.fut.as_mut().poll(cx);
        self.guard.heartbeat.0.store(0, Ordering::Relaxed);
        r
    }
}
