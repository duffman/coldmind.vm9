//! VM state, realm, public API.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::Rc;

use dj_bytecode as bc;

use crate::debugger::DebugState;
use crate::frame::{Frame, NativeFrame};
use crate::heap::{trace_value, Heap, HeapData, ObjRef};
use crate::jobs::{Job, Scheduler, Timer, VirtualClock};
use crate::loader::{load, LoadError, LoadedModule};
use crate::object::*;
use crate::value::{JsStr, PropKey, Symbol, Value};

#[derive(Clone, Debug)]
pub struct VmOptions {
    /// Maximum JavaScript call depth (RangeError beyond).
    pub max_frames: usize,
    /// Maximum nesting of native → JavaScript re-entry (bounds Rust stack use).
    pub max_native_depth: usize,
    /// Collect garbage at every safepoint (testing).
    pub gc_stress: bool,
    /// Capture console output in a buffer instead of writing to stdout.
    pub capture_output: bool,
}

impl Default for VmOptions {
    fn default() -> Self {
        VmOptions { max_frames: 10_000, max_native_depth: 400, gc_stress: false, capture_output: false }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorType {
    Error,
    TypeError,
    RangeError,
    ReferenceError,
    SyntaxError,
}

/// Well-known objects and symbols.
pub struct Realm {
    pub global: ObjRef,
    pub object_proto: ObjRef,
    pub function_proto: ObjRef,
    pub array_proto: ObjRef,
    pub string_proto: ObjRef,
    pub number_proto: ObjRef,
    pub boolean_proto: ObjRef,
    pub symbol_proto: ObjRef,
    pub error_proto: ObjRef,
    pub type_error_proto: ObjRef,
    pub range_error_proto: ObjRef,
    pub reference_error_proto: ObjRef,
    pub syntax_error_proto: ObjRef,
    pub promise_proto: ObjRef,
    pub map_proto: ObjRef,
    pub set_proto: ObjRef,
    pub iterator_proto: ObjRef,
    pub array_iter_proto: ObjRef,
    pub string_iter_proto: ObjRef,
    pub map_iter_proto: ObjRef,
    pub set_iter_proto: ObjRef,
    pub for_in_iter_proto: ObjRef,
    /// `%ArrayIteratorPrototype%.next` (enables the for-of fast path).
    pub array_iter_next: Option<ObjRef>,
    pub promise_ctor: Option<ObjRef>,
    pub sym_iterator: Symbol,
    pub sym_to_string_tag: Symbol,
    pub sym_has_instance: Symbol,
}

pub enum Output {
    Stdout,
    Buffer(String),
}

/// An exception that escaped to the host.
#[derive(Clone, Debug)]
pub struct Uncaught {
    pub value: Value,
    /// `Name: message` (or an inspected value).
    pub message: String,
    /// Stack trace text when the value is an Error.
    pub stack: Option<String>,
}

impl std::fmt::Display for Uncaught {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.stack {
            Some(s) => write!(f, "Uncaught {s}"),
            None => write!(f, "Uncaught {}", self.message),
        }
    }
}

impl std::error::Error for Uncaught {}

pub struct Vm {
    pub heap: Heap,
    pub realm: Realm,
    pub options: VmOptions,
    pub(crate) modules: Vec<Rc<LoadedModule>>,
    pub(crate) frames: Vec<Frame>,
    pub(crate) stack: Vec<Value>,
    pub(crate) roots: Vec<Value>,
    pub(crate) native_stack: Vec<NativeFrame>,
    pub(crate) jobs: VecDeque<Job>,
    pub(crate) timers: BTreeMap<(u64, u64), Timer>,
    pub(crate) timer_keys: HashMap<u64, (u64, u64)>,
    pub(crate) timer_seq: u64,
    pub(crate) scheduler: Box<dyn Scheduler>,
    /// Rejected promises without a handler (reported as unhandled rejections).
    pub(crate) rejections: Vec<ObjRef>,
    pub(crate) output: Output,
    pub(crate) debug: DebugState,
    pub(crate) next_frame_id: u64,
    /// Set when the debugger terminates execution: unwinding skips handlers.
    pub(crate) terminating: bool,
}

impl Vm {
    pub fn new(options: VmOptions) -> Vm {
        let mut heap = Heap::new();
        heap.stress = options.gc_stress;
        let obj = |heap: &mut Heap, proto: Option<ObjRef>, kind: ObjectKind| heap.alloc_object(JsObject::new(proto, kind));
        let object_proto = obj(&mut heap, None, ObjectKind::Ordinary);
        let op = Some(object_proto);
        let function_proto = obj(&mut heap, op, ObjectKind::Ordinary);
        let iterator_proto = obj(&mut heap, op, ObjectKind::Ordinary);
        let error_proto = obj(&mut heap, op, ObjectKind::Ordinary);
        let realm = Realm {
            global: obj(&mut heap, op, ObjectKind::Ordinary),
            object_proto,
            function_proto,
            array_proto: obj(&mut heap, op, ObjectKind::Array(Vec::new())),
            string_proto: obj(&mut heap, op, ObjectKind::StringBox(JsStr::default())),
            number_proto: obj(&mut heap, op, ObjectKind::NumberBox(0.0)),
            boolean_proto: obj(&mut heap, op, ObjectKind::BooleanBox(false)),
            symbol_proto: obj(&mut heap, op, ObjectKind::Ordinary),
            error_proto,
            type_error_proto: obj(&mut heap, Some(error_proto), ObjectKind::Ordinary),
            range_error_proto: obj(&mut heap, Some(error_proto), ObjectKind::Ordinary),
            reference_error_proto: obj(&mut heap, Some(error_proto), ObjectKind::Ordinary),
            syntax_error_proto: obj(&mut heap, Some(error_proto), ObjectKind::Ordinary),
            promise_proto: obj(&mut heap, op, ObjectKind::Ordinary),
            map_proto: obj(&mut heap, op, ObjectKind::Ordinary),
            set_proto: obj(&mut heap, op, ObjectKind::Ordinary),
            iterator_proto,
            array_iter_proto: obj(&mut heap, Some(iterator_proto), ObjectKind::Ordinary),
            string_iter_proto: obj(&mut heap, Some(iterator_proto), ObjectKind::Ordinary),
            map_iter_proto: obj(&mut heap, Some(iterator_proto), ObjectKind::Ordinary),
            set_iter_proto: obj(&mut heap, Some(iterator_proto), ObjectKind::Ordinary),
            for_in_iter_proto: obj(&mut heap, Some(iterator_proto), ObjectKind::Ordinary),
            array_iter_next: None,
            promise_ctor: None,
            sym_iterator: Symbol::new(Some(JsStr::new("Symbol.iterator"))),
            sym_to_string_tag: Symbol::new(Some(JsStr::new("Symbol.toStringTag"))),
            sym_has_instance: Symbol::new(Some(JsStr::new("Symbol.hasInstance"))),
        };
        let output = if options.capture_output { Output::Buffer(String::new()) } else { Output::Stdout };
        let mut vm = Vm {
            heap,
            realm,
            options,
            modules: Vec::new(),
            frames: Vec::new(),
            stack: Vec::new(),
            roots: Vec::new(),
            native_stack: Vec::new(),
            jobs: VecDeque::new(),
            timers: BTreeMap::new(),
            timer_keys: HashMap::new(),
            timer_seq: 0,
            scheduler: Box::new(VirtualClock::default()),
            rejections: Vec::new(),
            output,
            debug: DebugState::default(),
            next_frame_id: 1,
            terminating: false,
        };
        crate::builtins::setup(&mut vm);
        vm
    }

    /// Validate and load a bytecode module; returns its id.
    pub fn load(&mut self, m: bc::Module) -> Result<u32, LoadError> {
        let id = self.modules.len() as u32;
        let lm = load(m, id)?;
        self.modules.push(Rc::new(lm));
        Ok(id)
    }

    pub fn module(&self, id: u32) -> Option<&LoadedModule> {
        self.modules.get(id as usize).map(|m| &**m)
    }

    pub fn set_scheduler(&mut self, s: Box<dyn Scheduler>) {
        self.scheduler = s;
    }

    /// Run a module's top-level code, then the event loop until no work is left.
    pub fn run_module(&mut self, id: u32) -> Result<Value, Uncaught> {
        let v = self.run_script(id)?;
        self.run_event_loop()?;
        Ok(v)
    }

    /// Run a module's top-level code only (jobs stay queued).
    pub fn run_script(&mut self, id: u32) -> Result<Value, Uncaught> {
        let m = self.modules[id as usize].clone();
        let entry = m.bytecode.entry;
        let callee = self.make_closure(id, entry, Box::new([]));
        self.debug_on_start();
        let r = self
            .push_js_frame(callee, Value::Undefined, Value::Undefined, crate::interp::Args::Vec(Vec::new()), None, true, false)
            .and_then(|()| self.run_loop(None));
        r.map_err(|e| self.uncaught(e))
    }

    /// Convert an escaped exception into a host error.
    pub fn uncaught(&mut self, e: Value) -> Uncaught {
        if self.terminating {
            return Uncaught { value: Value::Undefined, message: "execution terminated by debugger".into(), stack: None };
        }
        let mut stack = None;
        let mut message = None;
        if let Value::Object(o) = &e {
            if matches!(self.heap.obj(*o).kind, ObjectKind::Error) {
                if let Some(Property { slot: Slot::Data(Value::String(s)), .. }) = self.heap.obj(*o).props.get(&PropKey::str("stack")) {
                    stack = Some(s.to_rust());
                }
                let name = self.get(&e, &PropKey::str("name")).ok().and_then(|v| self.to_string(&v).ok()).map(|s| s.to_rust());
                let msg = self.get(&e, &PropKey::str("message")).ok().and_then(|v| self.to_string(&v).ok()).map(|s| s.to_rust());
                message = Some(match (name, msg) {
                    (Some(n), Some(m)) if !m.is_empty() => format!("{n}: {m}"),
                    (Some(n), _) => n,
                    _ => "Error".into(),
                });
            }
        }
        let message = message.unwrap_or_else(|| crate::inspect::inspect(self, &e));
        Uncaught { value: e, message, stack }
    }

    pub fn take_output(&mut self) -> String {
        match &mut self.output {
            Output::Buffer(s) => std::mem::take(s),
            Output::Stdout => String::new(),
        }
    }

    pub fn write_output(&mut self, s: &str) {
        match &mut self.output {
            Output::Buffer(b) => b.push_str(s),
            Output::Stdout => {
                use std::io::Write;
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(s.as_bytes());
            }
        }
        self.debug_on_output(s);
    }

    /// `console.error` / `console.warn`: stderr, or the capture buffer.
    pub fn write_error_output(&mut self, s: &str) {
        match &mut self.output {
            Output::Buffer(b) => b.push_str(s),
            Output::Stdout => {
                use std::io::Write;
                let _ = std::io::stderr().lock().write_all(s.as_bytes());
            }
        }
        self.debug_on_output(s);
    }

    pub fn global(&self) -> ObjRef {
        self.realm.global
    }

    // --- rooting -------------------------------------------------------------

    /// Root `v` until `pop_roots` truncates below the returned mark.
    pub fn push_root(&mut self, v: Value) -> usize {
        self.roots.push(v);
        self.roots.len() - 1
    }

    pub fn root_mark(&self) -> usize {
        self.roots.len()
    }

    pub fn pop_roots(&mut self, mark: usize) {
        self.roots.truncate(mark);
    }

    // --- allocation helpers ----------------------------------------------------

    pub fn new_object(&mut self) -> ObjRef {
        let p = self.realm.object_proto;
        self.heap.alloc_object(JsObject::new(Some(p), ObjectKind::Ordinary))
    }

    pub fn new_object_with(&mut self, proto: Option<ObjRef>, kind: ObjectKind) -> ObjRef {
        self.heap.alloc_object(JsObject::new(proto, kind))
    }

    pub fn new_array(&mut self, elems: Vec<Value>) -> ObjRef {
        let p = self.realm.array_proto;
        self.heap.alloc_object(JsObject::new(Some(p), ObjectKind::Array(elems)))
    }

    pub fn new_native(&mut self, name: &str, length: u32, f: NativeFn) -> ObjRef {
        self.new_native_full(name, length, f, Vec::new(), false)
    }

    pub fn new_native_full(&mut self, name: &str, length: u32, f: NativeFn, data: Vec<Value>, constructor: bool) -> ObjRef {
        let fp = self.realm.function_proto;
        let mut o = JsObject::new(Some(fp), ObjectKind::Native(NativeFunction { f, data, constructor, name: JsStr::new(name) }));
        o.props.insert(PropKey::str("length"), Property::data(Value::Number(length as f64), CONFIGURABLE));
        o.props.insert(PropKey::str("name"), Property::data(Value::str(name), CONFIGURABLE));
        self.heap.alloc_object(o)
    }

    /// Define a builtin method (non-enumerable) and return the function.
    pub fn define_method(&mut self, obj: ObjRef, name: &str, length: u32, f: NativeFn) -> ObjRef {
        let func = self.new_native(name, length, f);
        self.heap.obj_mut(obj).props.insert(PropKey::str(name), Property::data(Value::Object(func), HIDDEN));
        func
    }

    pub fn define_symbol_method(&mut self, obj: ObjRef, sym: Symbol, name: &str, length: u32, f: NativeFn) -> ObjRef {
        let func = self.new_native(name, length, f);
        self.heap.obj_mut(obj).props.insert(PropKey::Sym(sym), Property::data(Value::Object(func), HIDDEN));
        func
    }

    pub fn define_value(&mut self, obj: ObjRef, key: impl Into<PropKey>, v: Value, flags: u8) {
        self.heap.obj_mut(obj).props.insert(key.into(), Property::data(v, flags));
    }

    pub fn define_getter(&mut self, obj: ObjRef, name: &str, f: NativeFn) {
        let g = self.new_native(&format!("get {name}"), 0, f);
        self.heap.obj_mut(obj).props.insert(PropKey::str(name), Property { slot: Slot::Accessor { get: Some(g), set: None }, flags: CONFIGURABLE });
    }

    /// Create a closure object for function `func` of module `module`.
    pub(crate) fn make_closure(&mut self, module: u32, func: u32, upvalues: Box<[ObjRef]>) -> ObjRef {
        let lf = self.modules[module as usize].functions[func as usize].clone();
        let fp = self.realm.function_proto;
        let mut o = JsObject::new(Some(fp), ObjectKind::Closure(Closure { module, func, upvalues, home_object: None, fields_init: None }));
        let visible_len = lf.param_count; // parameters before the first default/rest are not tracked separately
        o.props.insert(PropKey::str("length"), Property::data(Value::Number(visible_len as f64), CONFIGURABLE));
        o.props.insert(PropKey::str("name"), Property::data(Value::String(lf.name.clone()), CONFIGURABLE));
        let f = self.heap.alloc_object(o);
        if lf.kind == bc::FunctionKind::Normal && !lf.is_async {
            let proto = self.new_object();
            self.define_value(proto, "constructor", Value::Object(f), HIDDEN);
            self.define_value(f, "prototype", Value::Object(proto), WRITABLE);
        }
        f
    }

    // --- errors --------------------------------------------------------------

    pub fn error_proto(&self, t: ErrorType) -> ObjRef {
        match t {
            ErrorType::Error => self.realm.error_proto,
            ErrorType::TypeError => self.realm.type_error_proto,
            ErrorType::RangeError => self.realm.range_error_proto,
            ErrorType::ReferenceError => self.realm.reference_error_proto,
            ErrorType::SyntaxError => self.realm.syntax_error_proto,
        }
    }

    /// Create an Error object with `message` and a captured `stack`.
    pub fn make_error(&mut self, t: ErrorType, msg: &str) -> Value {
        let proto = self.error_proto(t);
        let o = self.new_object_with(Some(proto), ObjectKind::Error);
        if !msg.is_empty() {
            self.define_value(o, "message", Value::str(msg), HIDDEN);
        }
        let name = match t {
            ErrorType::Error => "Error",
            ErrorType::TypeError => "TypeError",
            ErrorType::RangeError => "RangeError",
            ErrorType::ReferenceError => "ReferenceError",
            ErrorType::SyntaxError => "SyntaxError",
        };
        let header = if msg.is_empty() { name.to_string() } else { format!("{name}: {msg}") };
        let stack = self.capture_stack(&header);
        self.define_value(o, "stack", Value::str(&stack), HIDDEN);
        Value::Object(o)
    }

    pub fn type_error(&mut self, msg: impl AsRef<str>) -> Value {
        self.make_error(ErrorType::TypeError, msg.as_ref())
    }

    pub fn range_error(&mut self, msg: impl AsRef<str>) -> Value {
        self.make_error(ErrorType::RangeError, msg.as_ref())
    }

    pub fn reference_error(&mut self, msg: impl AsRef<str>) -> Value {
        self.make_error(ErrorType::ReferenceError, msg.as_ref())
    }

    pub fn syntax_error(&mut self, msg: impl AsRef<str>) -> Value {
        self.make_error(ErrorType::SyntaxError, msg.as_ref())
    }

    /// Source position (`file:line:col`) of the instruction a frame is executing.
    pub fn frame_location(&self, f: &Frame) -> Option<(String, bc::Pos)> {
        let m = &self.modules[f.module as usize];
        let lf = &m.functions[f.func as usize];
        let at = f.pc.saturating_sub(1).min(lf.offsets.len().saturating_sub(1));
        let off = *lf.offsets.get(at)?;
        let pos = m.debug(f.func)?.pos_at(off)?;
        Some((m.source_name().to_string(), pos))
    }

    pub fn frame_function_name(&self, f: &Frame) -> String {
        let lf = &self.modules[f.module as usize].functions[f.func as usize];
        let n = lf.name.to_rust();
        if n.is_empty() { "<anonymous>".into() } else { n }
    }

    /// `header` followed by one `    at name (file:line:col)` line per frame,
    /// innermost first, natives interleaved.
    pub fn capture_stack(&self, header: &str) -> String {
        let mut out = header.to_string();
        let mut natives = self.native_stack.iter().rev().peekable();
        for i in (0..self.frames.len()).rev() {
            while let Some(n) = natives.next_if(|n| n.depth > i) {
                out.push_str(&format!("\n    at {} (native)", n.name));
            }
            let f = &self.frames[i];
            let name = self.frame_function_name(f);
            match self.frame_location(f) {
                Some((file, pos)) => out.push_str(&format!("\n    at {name} ({file}:{}:{})", pos.line, pos.col)),
                None => out.push_str(&format!("\n    at {name}")),
            }
        }
        for n in natives {
            out.push_str(&format!("\n    at {} (native)", n.name));
        }
        out
    }

    // --- garbage collection ------------------------------------------------------

    pub fn collect_garbage(&mut self) {
        let mut roots: Vec<ObjRef> = Vec::with_capacity(self.stack.len() + 64);
        for v in self.stack.iter().chain(self.roots.iter()) {
            trace_value(v, &mut roots);
        }
        for f in &self.frames {
            f.trace(&mut roots);
        }
        let r = &self.realm;
        roots.extend([
            r.global,
            r.object_proto,
            r.function_proto,
            r.array_proto,
            r.string_proto,
            r.number_proto,
            r.boolean_proto,
            r.symbol_proto,
            r.error_proto,
            r.type_error_proto,
            r.range_error_proto,
            r.reference_error_proto,
            r.syntax_error_proto,
            r.promise_proto,
            r.map_proto,
            r.set_proto,
            r.iterator_proto,
            r.array_iter_proto,
            r.string_iter_proto,
            r.map_iter_proto,
            r.set_iter_proto,
            r.for_in_iter_proto,
        ]);
        roots.extend(r.array_iter_next.iter().copied());
        roots.extend(r.promise_ctor.iter().copied());
        for j in &self.jobs {
            j.trace(&mut roots);
        }
        for t in self.timers.values() {
            trace_value(&t.callback, &mut roots);
            t.args.iter().for_each(|v| trace_value(v, &mut roots));
        }
        roots.extend(self.rejections.iter().copied());
        self.heap.collect(roots);
    }

    pub fn gc_stats(&self) -> crate::heap::GcStats {
        self.heap.stats
    }

    pub(crate) fn alloc_cell(&mut self, v: Value) -> ObjRef {
        self.heap.alloc(HeapData::Cell(v))
    }
}
