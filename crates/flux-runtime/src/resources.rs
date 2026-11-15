//! Per-extension resource accounting: things a limit has to count across commands.
//!
//! Limits are configured in `[limits]` (config.toml) and documented, with the reason for each
//! default, in `docs/resources.md`. A request over a limit is refused with a clear error; nothing
//! is queued silently.

use std::{
    cell::Cell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
};

use flux_api::{Limits, wire::Stats};

/// Counters another thread may read (the host's pong, `flux status`).
#[derive(Debug, Default)]
pub struct LiveStats {
    pub tasks: AtomicU32,
    pub http: AtomicU32,
    pub children: AtomicU32,
    pub lua_kb: AtomicU64,
}

impl LiveStats {
    pub fn snapshot(&self) -> Stats {
        Stats {
            tasks: self.tasks.load(Ordering::Relaxed),
            open_views: 0,
            http_in_flight: self.http.load(Ordering::Relaxed),
            children: self.children.load(Ordering::Relaxed),
            lua_memory_kb: self.lua_kb.load(Ordering::Relaxed),
        }
    }

    pub fn store(&self, s: &Stats) {
        self.tasks.store(s.tasks, Ordering::Relaxed);
        self.http.store(s.http_in_flight, Ordering::Relaxed);
        self.children.store(s.children, Ordering::Relaxed);
        self.lua_kb.store(s.lua_memory_kb, Ordering::Relaxed);
    }
}

impl std::fmt::Debug for Resources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Resources")
    }
}

pub struct Resources {
    pub max_http: usize,
    pub max_children: usize,
    pub exec_output_cap: usize,
    pub exec_timeout_max: f64,
    pub http_max_bytes: usize,
    http_inflight: Cell<usize>,
    children: Cell<usize>,
    own_pids: std::cell::RefCell<std::collections::HashSet<u32>>,
    pub live: Arc<LiveStats>,
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Http,
    Child,
}

/// Holds one unit of a limit until dropped.
/// Keeps a pid in the extension's own-process registry.
#[derive(Debug)]
pub struct PidGuard {
    res: Rc<Resources>,
    pid: u32,
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        self.res.own_pids.borrow_mut().remove(&self.pid);
    }
}

#[derive(Debug)]
pub struct Permit {
    res: Rc<Resources>,
    kind: Kind,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let (cell, live) = match self.kind {
            Kind::Http => (&self.res.http_inflight, &self.res.live.http),
            Kind::Child => (&self.res.children, &self.res.live.children),
        };
        cell.set(cell.get().saturating_sub(1));
        live.store(cell.get() as u32, Ordering::Relaxed);
    }
}

impl Resources {
    pub fn new(l: &Limits, live: Arc<LiveStats>) -> Rc<Self> {
        Rc::new(Self {
            max_http: l.max_http_concurrent,
            max_children: l.max_children,
            exec_output_cap: l.exec_output_kb * 1024,
            exec_timeout_max: l.exec_timeout_max_s as f64,
            http_max_bytes: l.http_max_mb * 1024 * 1024,
            http_inflight: Cell::new(0),
            children: Cell::new(0),
            own_pids: Default::default(),
            live,
        })
    }

    pub fn http_request(self: &Rc<Self>) -> Result<Permit, String> {
        if self.http_inflight.get() >= self.max_http {
            return Err(format!("too many requests in flight (max {}); wait for one to finish", self.max_http));
        }
        self.http_inflight.set(self.http_inflight.get() + 1);
        self.live.http.store(self.http_inflight.get() as u32, Ordering::Relaxed);
        Ok(Permit { res: self.clone(), kind: Kind::Http })
    }

    pub fn child(self: &Rc<Self>) -> Result<Permit, String> {
        if self.children.get() >= self.max_children {
            return Err(format!("too many background processes (max {}); wait for some to finish", self.max_children));
        }
        self.children.set(self.children.get() + 1);
        self.live.children.store(self.children.get() as u32, Ordering::Relaxed);
        Ok(Permit { res: self.clone(), kind: Kind::Child })
    }

    /// Remembers a process this extension started, until the guard is dropped.
    pub fn own_pid(self: &Rc<Self>, pid: u32) -> PidGuard {
        self.own_pids.borrow_mut().insert(pid);
        PidGuard { res: self.clone(), pid }
    }

    pub fn owns(&self, pid: u32) -> bool {
        self.own_pids.borrow().contains(&pid)
    }

    pub fn http_in_flight(&self) -> usize {
        self.http_inflight.get()
    }

    pub fn children_alive(&self) -> usize {
        self.children.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permits_are_counted_and_released() {
        let r = Resources::new(&Limits { max_children: 2, max_http_concurrent: 1, ..Limits::default() }, Default::default());
        let a = r.child().unwrap();
        let _b = r.child().unwrap();
        assert!(r.child().unwrap_err().contains("max 2"));
        drop(a);
        assert!(r.child().is_ok());
        let h = r.http_request().unwrap();
        assert!(r.http_request().is_err());
        drop(h);
        assert!(r.http_request().is_ok());
    }
}
