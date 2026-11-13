//! Promises, the job (microtask) queue, timers and the event loop.
//!
//! Scheduling interface: the host owns time through [`Scheduler`]. The VM
//! asks it for `now()` when arming timers and calls `wait_until(t)` before
//! firing the earliest timer. [`VirtualClock`] (the default) jumps straight
//! to `t`, which makes runs deterministic and instant; [`SystemClock`]
//! sleeps in real time.
//!
//! Event loop order: run the script; drain microtasks; report unhandled
//! rejections; then repeatedly fire the earliest timer (ties in creation
//! order) and drain microtasks again, until no timers remain.

use std::time::{Duration, Instant};

use crate::heap::{trace_value, ObjRef};
use crate::object::*;
use crate::value::{PropKey, Value};
use crate::vm::{Uncaught, Vm};

pub trait Scheduler {
    /// Current time in milliseconds.
    fn now(&self) -> u64;
    /// Block (or advance virtual time) until `t`.
    fn wait_until(&mut self, t: u64);
}

/// Deterministic virtual time: waiting is instantaneous.
#[derive(Default, Debug)]
pub struct VirtualClock {
    pub now: u64,
}

impl Scheduler for VirtualClock {
    fn now(&self) -> u64 {
        self.now
    }

    fn wait_until(&mut self, t: u64) {
        self.now = self.now.max(t);
    }
}

/// Wall-clock time.
pub struct SystemClock {
    start: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        SystemClock { start: Instant::now() }
    }
}

impl Scheduler for SystemClock {
    fn now(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    fn wait_until(&mut self, t: u64) {
        let now = self.now();
        if t > now {
            std::thread::sleep(Duration::from_millis(t - now));
        }
    }
}

pub(crate) enum Job {
    /// Run a promise reaction with the settled value.
    Reaction { reaction: Reaction, argument: Value, rejected: bool },
    /// NewPromiseResolveThenableJob.
    ResolveThenable { promise: ObjRef, thenable: Value, then: Value },
    /// `queueMicrotask(f)`.
    Callback { f: Value },
}

impl Job {
    pub(crate) fn trace(&self, out: &mut Vec<ObjRef>) {
        match self {
            Job::Reaction { reaction, argument, .. } => {
                match &reaction.kind {
                    ReactionKind::Handler(v) => trace_value(v, out),
                    ReactionKind::Resume(c) => out.push(*c),
                }
                out.extend(reaction.target.iter().copied());
                trace_value(argument, out);
            }
            Job::ResolveThenable { promise, thenable, then } => {
                out.push(*promise);
                trace_value(thenable, out);
                trace_value(then, out);
            }
            Job::Callback { f } => trace_value(f, out),
        }
    }
}

pub(crate) struct Timer {
    pub id: u64,
    pub callback: Value,
    pub args: Vec<Value>,
    /// Repeat period for `setInterval`.
    pub interval: Option<u64>,
}

impl Vm {
    // --- promise internals ---------------------------------------------------------

    pub fn new_promise(&mut self) -> ObjRef {
        let proto = Some(self.realm.promise_proto);
        self.new_object_with(
            proto,
            ObjectKind::Promise(PromiseData {
                state: PromiseState::Pending,
                fulfill_reactions: Vec::new(),
                reject_reactions: Vec::new(),
                handled: false,
            }),
        )
    }

    pub fn promise_state(&self, p: ObjRef) -> Option<&PromiseState> {
        match &self.heap.obj(p).kind {
            ObjectKind::Promise(d) => Some(&d.state),
            _ => None,
        }
    }

    fn promise_data(&mut self, p: ObjRef) -> &mut PromiseData {
        match &mut self.heap.obj_mut(p).kind {
            ObjectKind::Promise(d) => d,
            _ => unreachable!("not a promise"),
        }
    }

    /// Promise resolve function semantics (thenables are adopted).
    pub fn resolve_promise(&mut self, p: ObjRef, v: Value) {
        if !matches!(self.promise_data(p).state, PromiseState::Pending) {
            return;
        }
        if v.as_object() == Some(p) {
            let e = self.type_error("Chaining cycle detected for promise #<Promise>");
            self.reject_promise(p, e);
            return;
        }
        let Value::Object(o) = v else {
            self.fulfill_promise(p, v);
            return;
        };
        let mark = self.push_root(Value::Object(p));
        self.push_root(v.clone());
        let then = self.get_with_receiver(o, &PropKey::str("then"), &v);
        self.pop_roots(mark);
        match then {
            Err(e) => self.reject_promise(p, e),
            Ok(then) if self.is_callable(&then) => {
                self.jobs.push_back(Job::ResolveThenable { promise: p, thenable: v, then });
            }
            Ok(_) => self.fulfill_promise(p, v),
        }
    }

    fn fulfill_promise(&mut self, p: ObjRef, v: Value) {
        let d = self.promise_data(p);
        d.state = PromiseState::Fulfilled(v.clone());
        let reactions = std::mem::take(&mut d.fulfill_reactions);
        d.reject_reactions.clear();
        for r in reactions {
            self.jobs.push_back(Job::Reaction { reaction: r, argument: v.clone(), rejected: false });
        }
    }

    pub fn reject_promise(&mut self, p: ObjRef, e: Value) {
        let d = self.promise_data(p);
        if !matches!(d.state, PromiseState::Pending) {
            return;
        }
        d.state = PromiseState::Rejected(e.clone());
        let reactions = std::mem::take(&mut d.reject_reactions);
        d.fulfill_reactions.clear();
        if !d.handled {
            self.rejections.push(p);
        }
        for r in reactions {
            self.jobs.push_back(Job::Reaction { reaction: r, argument: e.clone(), rejected: true });
        }
    }

    /// PerformPromiseThen with explicit reactions.
    pub(crate) fn perform_then(&mut self, p: ObjRef, on_fulfilled: ReactionKind, on_rejected: ReactionKind, target: Option<ObjRef>) {
        let f = Reaction { kind: on_fulfilled, target };
        let r = Reaction { kind: on_rejected, target };
        let d = self.promise_data(p);
        let was_handled = d.handled;
        d.handled = true;
        match d.state.clone() {
            PromiseState::Pending => {
                d.fulfill_reactions.push(f);
                d.reject_reactions.push(r);
            }
            PromiseState::Fulfilled(v) => self.jobs.push_back(Job::Reaction { reaction: f, argument: v, rejected: false }),
            PromiseState::Rejected(e) => {
                if !was_handled {
                    self.rejections.retain(|&x| x != p);
                }
                self.jobs.push_back(Job::Reaction { reaction: r, argument: e, rejected: true });
            }
        }
    }

    /// PromiseResolve(%Promise%, v) for `await`: native promises are used
    /// as they are, other values are wrapped.
    pub(crate) fn promise_resolve_value(&mut self, v: Value) -> ObjRef {
        if let Value::Object(o) = &v {
            if matches!(self.heap.obj(*o).kind, ObjectKind::Promise(_)) {
                let ctor = self.get_own_property(self.heap.obj(*o).proto.unwrap_or(*o), &PropKey::str("constructor"));
                let is_intrinsic = matches!(
                    (ctor, self.realm.promise_ctor),
                    (Some(Property { slot: Slot::Data(Value::Object(c)), .. }), Some(pc)) if c == pc
                );
                if is_intrinsic {
                    return *o;
                }
            }
        }
        let p = self.new_promise();
        let mark = self.push_root(Value::Object(p));
        self.resolve_promise(p, v);
        self.pop_roots(mark);
        p
    }

    pub(crate) fn promise_then_resume(&mut self, p: ObjRef, coro: ObjRef) {
        self.perform_then(p, ReactionKind::Resume(coro), ReactionKind::Resume(coro), None);
    }

    // --- job execution ---------------------------------------------------------------

    fn run_job(&mut self, job: Job) -> Result<(), Value> {
        match job {
            Job::Reaction { reaction, argument, rejected } => match reaction.kind {
                ReactionKind::Resume(coro) => {
                    let r = if rejected { Err(argument) } else { Ok(argument) };
                    self.resume_coroutine(coro, r)
                }
                ReactionKind::Handler(h) => {
                    let mark = self.push_root(h.clone());
                    self.push_root(argument.clone());
                    if let Some(t) = reaction.target {
                        self.push_root(Value::Object(t));
                    }
                    let result = if h.is_undefined() {
                        if rejected { Err(argument) } else { Ok(argument) }
                    } else {
                        self.call(&h, Value::Undefined, vec![argument])
                    };
                    self.pop_roots(mark);
                    if self.terminating {
                        return Err(Value::Undefined);
                    }
                    match (reaction.target, result) {
                        (Some(t), Ok(v)) => self.resolve_promise(t, v),
                        (Some(t), Err(e)) => self.reject_promise(t, e),
                        (None, r) => {
                            r?;
                        }
                    }
                    Ok(())
                }
            },
            Job::ResolveThenable { promise, thenable, then } => {
                let mark = self.push_root(Value::Object(promise));
                let (res, rej) = crate::builtins::promise::resolving_functions(self, promise);
                self.push_root(Value::Object(res));
                self.push_root(Value::Object(rej));
                let r = self.call(&then, thenable, vec![Value::Object(res), Value::Object(rej)]);
                if let Err(e) = r {
                    if self.terminating {
                        self.pop_roots(mark);
                        return Err(e);
                    }
                    let _ = self.call(&Value::Object(rej), Value::Undefined, vec![e]);
                }
                self.pop_roots(mark);
                Ok(())
            }
            Job::Callback { f } => self.call(&f, Value::Undefined, Vec::new()).map(|_| ()),
        }
    }

    /// Drain the microtask queue.
    pub fn run_microtasks(&mut self) -> Result<(), Uncaught> {
        while let Some(job) = self.jobs.pop_front() {
            if let Err(e) = self.run_job(job) {
                return Err(self.uncaught(e));
            }
        }
        Ok(())
    }

    fn check_rejections(&mut self) -> Result<(), Uncaught> {
        let Some(&p) = self.rejections.first() else { return Ok(()) };
        self.rejections.clear();
        let e = match self.promise_state(p) {
            Some(PromiseState::Rejected(e)) => e.clone(),
            _ => Value::Undefined,
        };
        let mut u = self.uncaught(e);
        u.message = format!("(in promise) {}", u.message);
        if let Some(s) = &mut u.stack {
            *s = format!("(in promise) {s}");
        }
        Err(u)
    }

    /// Run queued microtasks and timers until no work is left.
    pub fn run_event_loop(&mut self) -> Result<(), Uncaught> {
        loop {
            self.run_microtasks()?;
            self.check_rejections()?;
            let Some((&key, _)) = self.timers.first_key_value() else { return Ok(()) };
            self.scheduler.wait_until(key.0);
            let t = self.timers.remove(&key).expect("timer");
            self.timer_keys.remove(&t.id);
            if let Some(iv) = t.interval {
                let at = self.scheduler.now().max(key.0) + iv;
                self.timer_seq += 1;
                let k = (at, self.timer_seq);
                self.timer_keys.insert(t.id, k);
                self.timers.insert(k, Timer { id: t.id, callback: t.callback.clone(), args: t.args.clone(), interval: t.interval });
            }
            if let Err(e) = self.call(&t.callback, Value::Undefined, t.args) {
                return Err(self.uncaught(e));
            }
        }
    }

    /// Arm a timer; returns its id.
    pub(crate) fn add_timer(&mut self, callback: Value, delay: f64, args: Vec<Value>, repeat: bool) -> u64 {
        let delay = if delay.is_finite() && delay > 0.0 { delay as u64 } else { 0 };
        let at = self.scheduler.now() + delay;
        self.timer_seq += 1;
        let id = self.timer_seq;
        let key = (at, id);
        self.timers.insert(key, Timer { id, callback, args, interval: repeat.then_some(delay.max(1)) });
        self.timer_keys.insert(id, key);
        id
    }

    pub(crate) fn clear_timer(&mut self, id: u64) {
        if let Some(k) = self.timer_keys.remove(&id) {
            self.timers.remove(&k);
        }
    }

    pub(crate) fn enqueue_callback(&mut self, f: Value) {
        self.jobs.push_back(Job::Callback { f });
    }

    /// Number of pending jobs and timers (host introspection).
    pub fn pending_work(&self) -> (usize, usize) {
        (self.jobs.len(), self.timers.len())
    }
}
