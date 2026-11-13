//! Garbage-collected heap.
//!
//! Objects, binding cells and suspended async activations live in a slot
//! vector addressed by generational handles ([`ObjRef`]). Collection is a
//! precise, non-moving mark-sweep:
//!
//! * **Roots**: every register of every frame, frame metadata (callee,
//!   `this`, `new.target`, pending exception), the native root stack, the
//!   global object and intrinsics, queued jobs and timers, tracked
//!   rejections. See `Vm::collect_garbage`.
//! * **Safepoints**: collection only starts at instruction boundaries in the
//!   interpreter loop, never inside an allocation, so Rust code between two
//!   safepoints may hold unrooted handles freely. Native functions that call
//!   back into JavaScript (which reaches a safepoint) root what they hold.
//! * **Safety**: no `unsafe`. A handle to a freed slot fails the generation
//!   check and panics with "stale handle" instead of aliasing a new object;
//!   the GC-stress test mode (collect at every safepoint) turns any rooting
//!   bug into such a deterministic failure.

use crate::frame::Coroutine;
use crate::object::{JsObject, ObjectKind, ReactionKind, Slot};
use crate::value::Value;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjRef {
    idx: u32,
    generation:u32,
}

impl std::fmt::Debug for ObjRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#{}.{}", self.idx, self.generation)
    }
}

impl ObjRef {
    pub fn index(self) -> u32 {
        self.idx
    }
}

pub enum HeapData {
    Object(JsObject),
    /// A captured variable binding.
    Cell(Value),
    /// A suspended async function activation.
    Coroutine(Box<Coroutine>),
}

struct HeapSlot {
    generation:u32,
    marked: bool,
    data: Option<HeapData>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GcStats {
    pub collections: u64,
    pub freed: u64,
    pub allocated: u64,
}

pub struct Heap {
    slots: Vec<HeapSlot>,
    free: Vec<u32>,
    live: usize,
    since_gc: usize,
    threshold: usize,
    pub stress: bool,
    pub stats: GcStats,
}

const MIN_THRESHOLD: usize = 4096;

impl Heap {
    pub fn new() -> Self {
        Heap { slots: Vec::new(), free: Vec::new(), live: 0, since_gc: 0, threshold: MIN_THRESHOLD, stress: false, stats: GcStats::default() }
    }

    pub fn alloc(&mut self, data: HeapData) -> ObjRef {
        self.live += 1;
        self.since_gc += 1;
        self.stats.allocated += 1;
        if let Some(idx) = self.free.pop() {
            let s = &mut self.slots[idx as usize];
            s.data = Some(data);
            ObjRef { idx, generation:s.generation }
        } else {
            self.slots.push(HeapSlot { generation:0, marked: false, data: Some(data) });
            ObjRef { idx: self.slots.len() as u32 - 1, generation:0 }
        }
    }

    pub fn alloc_object(&mut self, o: JsObject) -> ObjRef {
        self.alloc(HeapData::Object(o))
    }

    pub fn is_live(&self, r: ObjRef) -> bool {
        self.slots.get(r.idx as usize).is_some_and(|s| s.generation == r.generation && s.data.is_some())
    }

    #[inline]
    pub fn get(&self, r: ObjRef) -> &HeapData {
        match self.slots.get(r.idx as usize) {
            Some(s) if s.generation == r.generation => s.data.as_ref().expect("stale handle (freed slot)"),
            _ => panic!("stale handle {r:?}"),
        }
    }

    #[inline]
    pub fn get_mut(&mut self, r: ObjRef) -> &mut HeapData {
        match self.slots.get_mut(r.idx as usize) {
            Some(s) if s.generation == r.generation => s.data.as_mut().expect("stale handle (freed slot)"),
            _ => panic!("stale handle {r:?}"),
        }
    }

    #[inline]
    pub fn obj(&self, r: ObjRef) -> &JsObject {
        match self.get(r) {
            HeapData::Object(o) => o,
            _ => panic!("heap slot {r:?} is not an object"),
        }
    }

    #[inline]
    pub fn obj_mut(&mut self, r: ObjRef) -> &mut JsObject {
        match self.get_mut(r) {
            HeapData::Object(o) => o,
            _ => panic!("heap slot {r:?} is not an object"),
        }
    }

    /// `None` when `r` is not a cell (used by the debugger on frames whose
    /// cell has not been created yet).
    pub fn try_cell(&self, r: ObjRef) -> Option<&Value> {
        match self.slots.get(r.idx as usize) {
            Some(s) if s.generation == r.generation => match &s.data {
                Some(HeapData::Cell(v)) => Some(v),
                _ => None,
            },
            _ => None,
        }
    }

    pub fn is_cell(&self, r: ObjRef) -> bool {
        self.try_cell(r).is_some()
    }

    #[inline]
    pub fn cell(&self, r: ObjRef) -> &Value {
        match self.get(r) {
            HeapData::Cell(v) => v,
            _ => panic!("heap slot {r:?} is not a cell"),
        }
    }

    #[inline]
    pub fn cell_mut(&mut self, r: ObjRef) -> &mut Value {
        match self.get_mut(r) {
            HeapData::Cell(v) => v,
            _ => panic!("heap slot {r:?} is not a cell"),
        }
    }

    pub fn live_count(&self) -> usize {
        self.live
    }

    pub fn should_collect(&self) -> bool {
        self.stress || self.since_gc >= self.threshold
    }

    /// Mark from `roots`, sweep everything unmarked.
    pub fn collect(&mut self, roots: Vec<ObjRef>) {
        let mut work = roots;
        while let Some(r) = work.pop() {
            let Some(slot) = self.slots.get_mut(r.idx as usize) else { continue };
            if slot.generation != r.generation || slot.marked || slot.data.is_none() {
                continue;
            }
            slot.marked = true;
            trace(slot.data.as_ref().unwrap(), &mut work);
        }
        let mut freed = 0;
        for (i, s) in self.slots.iter_mut().enumerate() {
            if s.data.is_none() {
                continue;
            }
            if s.marked {
                s.marked = false;
            } else {
                s.data = None;
                s.generation = s.generation.wrapping_add(1);
                self.free.push(i as u32);
                freed += 1;
            }
        }
        self.live -= freed;
        self.since_gc = 0;
        self.threshold = (self.live * 2).max(MIN_THRESHOLD);
        self.stats.collections += 1;
        self.stats.freed += freed as u64;
    }
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

pub fn trace_value(v: &Value, out: &mut Vec<ObjRef>) {
    if let Value::Object(o) = v {
        out.push(*o);
    }
}

fn trace(d: &HeapData, out: &mut Vec<ObjRef>) {
    match d {
        HeapData::Cell(v) => trace_value(v, out),
        HeapData::Coroutine(c) => c.trace(out),
        HeapData::Object(o) => {
            if let Some(p) = o.proto {
                out.push(p);
            }
            for p in o.props.values() {
                match &p.slot {
                    Slot::Data(v) => trace_value(v, out),
                    Slot::Accessor { get, set } => {
                        out.extend(get.iter().copied());
                        out.extend(set.iter().copied());
                    }
                }
            }
            match &o.kind {
                ObjectKind::Ordinary
                | ObjectKind::Error
                | ObjectKind::BooleanBox(_)
                | ObjectKind::NumberBox(_)
                | ObjectKind::StringBox(_)
                | ObjectKind::SymbolBox(_)
                | ObjectKind::StringIter { .. } => {}
                ObjectKind::Array(els) => els.iter().for_each(|v| trace_value(v, out)),
                ObjectKind::Closure(c) => {
                    out.extend(c.upvalues.iter().copied());
                    out.extend(c.home_object.iter().copied());
                    out.extend(c.fields_init.iter().copied());
                }
                ObjectKind::Native(n) => n.data.iter().for_each(|v| trace_value(v, out)),
                ObjectKind::Promise(p) => {
                    match &p.state {
                        crate::object::PromiseState::Fulfilled(v) | crate::object::PromiseState::Rejected(v) => trace_value(v, out),
                        crate::object::PromiseState::Pending => {}
                    }
                    for r in p.fulfill_reactions.iter().chain(&p.reject_reactions) {
                        match &r.kind {
                            ReactionKind::Handler(v) => trace_value(v, out),
                            ReactionKind::Resume(c) => out.push(*c),
                        }
                        out.extend(r.target.iter().copied());
                    }
                }
                ObjectKind::Map(m) | ObjectKind::Set(m) => {
                    for (k, v) in m.entries.iter().flatten() {
                        trace_value(k, out);
                        trace_value(v, out);
                    }
                }
                ObjectKind::ArrayIter { target, .. } => trace_value(target, out),
                ObjectKind::CollectionIter { coll, .. } => out.push(*coll),
                ObjectKind::ForInIter { obj, .. } => out.extend(obj.iter().copied()),
            }
        }
    }
}
