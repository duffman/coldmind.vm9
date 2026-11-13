//! Debugger interface.
//!
//! The host installs a [`DebugHandler`]. The interpreter calls it
//! synchronously whenever execution stops (breakpoint, step, pause request,
//! `debugger;` statement, exception, entry). While the handler runs, the VM
//! is paused at a well-defined safepoint and the handler inspects state
//! through a [`DebugView`] (frames, scopes, variables, evaluation, exception
//! info). Inspection never runs JavaScript (getters are reported, not
//! invoked), so it cannot change program state or trigger collection.
//! The handler's [`ResumeAction`] decides how execution continues.
//!
//! Stop locations are statement starts (`is_stmt` line-table entries). The
//! interpreter checks, at each instruction while a handler is installed:
//! the asynchronous pause flag ([`Vm::pause_handle`]), then on statement
//! starts the active step request and breakpoints keyed by
//! `(module, function, instruction)`.
//!
//! Stepping semantics:
//! * `StepIn`: stop at the next statement start in any frame.
//! * `StepOver`: stop at the next statement start in the same activation
//!   (identified by frame id, so stepping over `await` resumes in the same
//!   async function); when that activation returns, continue in its caller.
//! * `StepOut`: stop at the next statement start in the caller.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use dj_bytecode::{FunctionDebug, IrPos, Pos, ScopeKind, VarDebug, VarLocation};

use crate::frame::Frame;
use crate::heap::ObjRef;
use crate::loader::LoadedFunction;
use crate::object::*;
use crate::value::{JsStr, PropKey, Value};
use crate::vm::Vm;

#[derive(Clone, Debug, PartialEq)]
pub enum StopReason {
    /// First statement of the script (`stop_on_entry`).
    Entry,
    /// Breakpoint ids hit at this location.
    Breakpoint(Vec<u32>),
    Step,
    Pause,
    DebuggerStatement,
    Exception { uncaught: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeAction {
    Continue,
    StepIn,
    StepOver,
    StepOut,
    /// Abort execution: the run returns an "execution terminated" error.
    Terminate,
}

pub trait DebugHandler {
    fn on_stop(&mut self, view: &mut DebugView<'_>, reason: StopReason) -> ResumeAction;
    /// Program output (console), in order with stops.
    fn on_output(&mut self, _text: &str) {}
}

#[derive(Clone, Debug)]
pub struct BreakpointSpec {
    pub line: u32,
    pub column: Option<u32>,
    /// Stop only when this expression (see [`DebugView::evaluate`]) is truthy.
    pub condition: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Breakpoint {
    pub id: u32,
    /// Resolved to a statement location.
    pub verified: bool,
    /// Actual location (the request's when unverified).
    pub line: u32,
    pub column: u32,
    pub func: Option<u32>,
    /// Bytecode offset of the statement start.
    pub pc: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExceptionFilters {
    /// Stop on every throw.
    pub all: bool,
    /// Stop on throws no enclosing handler (in any frame) catches.
    pub uncaught: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StepKind {
    In,
    Over,
    Out,
    /// The stepped frame returned: stop at the caller's next instruction
    /// (the return point), statement start or not.
    Return,
}

#[derive(Clone, Copy, Debug)]
struct StepRequest {
    kind: StepKind,
    frame_id: u64,
}

struct BpEntry {
    id: u32,
    condition: Option<String>,
}

#[derive(Default)]
pub(crate) struct DebugState {
    /// A handler is installed: the interpreter calls the safepoint hook.
    pub(crate) active: bool,
    handler: Option<Box<dyn DebugHandler>>,
    pause: Arc<AtomicBool>,
    breakpoints: HashMap<(u32, u32, u32), Vec<BpEntry>>,
    next_bp_id: u32,
    step: Option<StepRequest>,
    stop_on_entry: bool,
    entry_pending: bool,
    filters: ExceptionFilters,
    /// Last exception reported (rethrows of the same value are not re-reported).
    last_exception: Option<Value>,
    /// Exception being reported by the current stop.
    current_exception: Option<(Value, bool)>,
}

impl Vm {
    // --- host API ---------------------------------------------------------------------

    pub fn set_debug_handler(&mut self, h: Box<dyn DebugHandler>) {
        self.debug.handler = Some(h);
        self.debug.active = true;
    }

    pub fn clear_debug_handler(&mut self) -> Option<Box<dyn DebugHandler>> {
        self.debug.active = false;
        self.debug.step = None;
        self.debug.handler.take()
    }

    /// Setting the flag (from any thread) pauses at the next instruction.
    pub fn pause_handle(&self) -> Arc<AtomicBool> {
        self.debug.pause.clone()
    }

    pub fn set_stop_on_entry(&mut self, on: bool) {
        self.debug.stop_on_entry = on;
    }

    pub fn set_exception_filters(&mut self, f: ExceptionFilters) {
        self.debug.filters = f;
    }

    /// Replace all breakpoints of `module` (DAP `setBreakpoints` semantics).
    pub fn set_breakpoints(&mut self, module: u32, specs: &[BreakpointSpec]) -> Vec<Breakpoint> {
        self.debug.breakpoints.retain(|k, _| k.0 != module);
        let m = self.modules[module as usize].clone();
        let mut out = Vec::new();
        for s in specs {
            self.debug.next_bp_id += 1;
            let id = self.debug.next_bp_id;
            let loc = m.bytecode.debug.as_ref().and_then(|d| d.resolve_breakpoint(s.line, s.column));
            let resolved = loc.and_then(|l| {
                let lf = &m.functions[l.func as usize];
                lf.index_of_offset(l.pc).map(|idx| (l, idx as u32))
            });
            match resolved {
                Some((l, idx)) => {
                    self.debug.breakpoints.entry((module, l.func, idx)).or_default().push(BpEntry { id, condition: s.condition.clone() });
                    out.push(Breakpoint { id, verified: true, line: l.pos.line, column: l.pos.col, func: Some(l.func), pc: Some(l.pc) });
                }
                None => out.push(Breakpoint { id, verified: false, line: s.line, column: s.column.unwrap_or(1), func: None, pc: None }),
            }
        }
        out
    }

    // --- interpreter hooks ----------------------------------------------------------

    pub(crate) fn debug_on_start(&mut self) {
        if self.debug.active && self.debug.stop_on_entry {
            self.debug.step = Some(StepRequest { kind: StepKind::In, frame_id: 0 });
            self.debug.entry_pending = true;
        }
    }

    pub(crate) fn debug_on_output(&mut self, s: &str) {
        if let Some(h) = self.debug.handler.as_mut() {
            h.on_output(s);
        }
    }

    pub(crate) fn debug_on_return(&mut self, frame: &Frame) {
        let Some(st) = self.debug.step else { return };
        if st.frame_id == frame.id {
            // Stop where the caller resumes.
            self.debug.step = self.frames.last().map(|f| StepRequest { kind: StepKind::Return, frame_id: f.id });
        }
    }

    /// Called before executing instruction `pc` of frame `fi`. Returns `true`
    /// when the debugger requested termination.
    pub(crate) fn debug_safepoint(&mut self, fi: usize, lf: &LoadedFunction, pc: usize) -> bool {
        let paused = self.debug.pause.swap(false, Ordering::Relaxed);
        let returning = self.debug.step.is_some_and(|s| s.kind == StepKind::Return);
        if !paused && !lf.stmt[pc] && !returning {
            return false;
        }
        let f = &self.frames[fi];
        let step_hit = !paused
            && self.debug.step.is_some_and(|s| match s.kind {
                StepKind::In => lf.stmt[pc],
                StepKind::Over => lf.stmt[pc] && s.frame_id == f.id,
                StepKind::Out => false,
                StepKind::Return => s.frame_id == f.id,
            });
        let key = (f.module, f.func, pc as u32);
        let has_bp = !paused && self.debug.breakpoints.contains_key(&key);
        if !paused && !step_hit && !has_bp {
            return false;
        }
        // While stopped, `pc - 1` is the current instruction in every frame.
        // At a return point that is the call that just returned.
        let at_return = step_hit && self.debug.step.is_some_and(|s| s.kind == StepKind::Return) && pc > 0;
        self.frames[fi].pc = if at_return { pc } else { pc + 1 };
        let mut reason = if paused {
            Some(StopReason::Pause)
        } else if step_hit {
            Some(if std::mem::take(&mut self.debug.entry_pending) { StopReason::Entry } else { StopReason::Step })
        } else {
            None
        };
        if has_bp {
            let entries: Vec<(u32, Option<String>)> =
                self.debug.breakpoints[&key].iter().map(|e| (e.id, e.condition.clone())).collect();
            let mut ids = Vec::new();
            for (id, cond) in entries {
                let ok = match cond {
                    None => true,
                    Some(c) => DebugView::new(self).evaluate_value(0, &c).map(|v| v.truthy()).unwrap_or(true),
                };
                if ok {
                    ids.push(id);
                }
            }
            if !ids.is_empty() {
                reason = Some(StopReason::Breakpoint(ids));
            }
        }
        let action = match reason {
            Some(r) => self.debug_stop(r),
            None => ResumeAction::Continue,
        };
        self.frames[fi].pc = pc;
        action == ResumeAction::Terminate
    }

    /// `debugger;` statement (the frame's pc is already past it).
    pub(crate) fn debug_statement(&mut self, _fi: usize, _lf: &LoadedFunction) -> bool {
        self.debug_stop(StopReason::DebuggerStatement) == ResumeAction::Terminate
    }

    pub(crate) fn debug_on_throw(&mut self, exc: &Value) {
        let filters = self.debug.filters;
        if !self.debug.active || self.terminating || !(filters.all || filters.uncaught) {
            return;
        }
        if self.debug.last_exception.as_ref().is_some_and(|l| l.strict_equals(exc)) {
            return;
        }
        let uncaught = !self.exception_is_caught();
        if !(filters.all || uncaught) {
            return;
        }
        self.debug.last_exception = Some(exc.clone());
        self.debug.current_exception = Some((exc.clone(), uncaught));
        let action = self.debug_stop(StopReason::Exception { uncaught });
        self.debug.current_exception = None;
        if action == ResumeAction::Terminate {
            self.terminating = true;
        }
    }

    /// Some frame has a handler covering its current instruction, or an
    /// async function will turn the throw into a rejection.
    fn exception_is_caught(&self) -> bool {
        self.frames.iter().any(|f| {
            let lf = &self.modules[f.module as usize].functions[f.func as usize];
            lf.handler_for(f.pc.saturating_sub(1)).is_some() || f.async_promise.is_some()
        })
    }

    fn debug_stop(&mut self, reason: StopReason) -> ResumeAction {
        let Some(mut h) = self.debug.handler.take() else { return ResumeAction::Continue };
        self.debug.step = None;
        self.debug.entry_pending = false;
        let action = {
            let mut view = DebugView::new(self);
            h.on_stop(&mut view, reason)
        };
        self.debug.handler = Some(h);
        let top = self.frames.last().map_or(0, |f| f.id);
        let kind = match action {
            ResumeAction::StepIn => Some(StepKind::In),
            ResumeAction::StepOver => Some(StepKind::Over),
            ResumeAction::StepOut => Some(StepKind::Out),
            _ => None,
        };
        self.debug.step = kind.map(|kind| StepRequest { kind, frame_id: top });
        action
    }
}

// --- inspection ------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub struct FrameInfo {
    /// 0 = innermost.
    pub index: usize,
    /// Stable activation id (survives `await` suspension).
    pub id: u64,
    pub name: String,
    pub module: u32,
    pub func: u32,
    /// Bytecode offset of the current instruction.
    pub pc: u32,
    pub source: String,
    /// 1-based; `None` without debug info.
    pub pos: Option<Pos>,
    /// IR block / instruction the current instruction was generated from.
    pub ir: Option<IrPos>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ScopeInfo {
    /// "Local", "Block", "Catch", "Class", "Closure" or "Global".
    pub name: String,
    pub kind: Option<ScopeKind>,
    pub variables_ref: u32,
    pub range: Option<(Pos, Pos)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Variable {
    pub name: String,
    /// Short display form.
    pub value: String,
    /// `typeof`-style type ("uninitialized" for bindings in their TDZ).
    pub type_name: String,
    /// Declaration kind for bindings (`let`, `const`, `param`, ...).
    pub kind: Option<String>,
    /// Non-zero when the value has children (pass to [`DebugView::variables`]).
    pub variables_ref: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExceptionInfo {
    pub value: Variable,
    /// `Name: message` for errors, otherwise the displayed value.
    pub description: String,
    pub stack: Option<String>,
    pub uncaught: bool,
}

#[derive(Clone, Copy, Debug)]
enum RefTarget {
    /// Bindings of a lexical scope (`None` = closure upvalues) of a frame.
    Scope { frame: usize, scope: Option<u32> },
    Global,
    Object(ObjRef),
}

/// View of a paused VM: inspection (never runs JavaScript) plus breakpoint
/// and exception-filter configuration. Variable references are valid until
/// the handler returns.
pub struct DebugView<'a> {
    vm: &'a mut Vm,
    refs: Vec<RefTarget>,
}

impl<'a> DebugView<'a> {
    fn new(vm: &'a mut Vm) -> Self {
        DebugView { vm, refs: Vec::new() }
    }

    pub fn vm(&self) -> &Vm {
        self.vm
    }

    /// Replace a module's breakpoints while stopped.
    pub fn set_breakpoints(&mut self, module: u32, specs: &[BreakpointSpec]) -> Vec<Breakpoint> {
        self.vm.set_breakpoints(module, specs)
    }

    pub fn set_exception_filters(&mut self, f: ExceptionFilters) {
        self.vm.set_exception_filters(f);
    }

    fn new_ref(&mut self, t: RefTarget) -> u32 {
        self.refs.push(t);
        self.refs.len() as u32
    }

    /// Frame by index (0 = innermost).
    fn frame(&self, index: usize) -> Option<&Frame> {
        let n = self.vm.frames.len();
        if index < n { Some(&self.vm.frames[n - 1 - index]) } else { None }
    }

    fn frame_debug(&self, f: &Frame) -> (Option<&FunctionDebug>, u32) {
        let m = &self.vm.modules[f.module as usize];
        let lf = &m.functions[f.func as usize];
        let at = f.pc.saturating_sub(1).min(lf.offsets.len().saturating_sub(1));
        (m.debug(f.func), lf.offsets.get(at).copied().unwrap_or(0))
    }

    pub fn frame_count(&self) -> usize {
        self.vm.frames.len()
    }

    pub fn frames(&self) -> Vec<FrameInfo> {
        (0..self.vm.frames.len())
            .map(|i| {
                let f = self.frame(i).unwrap();
                let (d, pc) = self.frame_debug(f);
                FrameInfo {
                    index: i,
                    id: f.id,
                    name: self.vm.frame_function_name(f),
                    module: f.module,
                    func: f.func,
                    pc,
                    source: self.vm.modules[f.module as usize].source_name().to_string(),
                    pos: d.and_then(|d| d.pos_at(pc)),
                    ir: d.and_then(|d| d.ir_pos_at(pc)),
                }
            })
            .collect()
    }

    /// Bindings visible in a frame, innermost scope first.
    fn visible(&self, frame: usize) -> Vec<(&VarDebug, Option<u32>)> {
        let Some(f) = self.frame(frame) else { return Vec::new() };
        let (Some(d), pc) = self.frame_debug(f) else { return Vec::new() };
        let chain = d.scope_chain(pc);
        let mut vars: Vec<(&VarDebug, usize)> = d
            .visible_variables(pc)
            .map(|v| {
                let rank = match v.scope {
                    Some(s) => chain.iter().position(|&c| c == s).unwrap_or(chain.len()),
                    None => chain.len() + 1,
                };
                (v, rank)
            })
            .collect();
        vars.sort_by_key(|&(_, r)| r);
        vars.into_iter().map(|(v, _)| (v, v.scope)).collect()
    }

    pub fn scopes(&mut self, frame: usize) -> Vec<ScopeInfo> {
        let Some(f) = self.frame(frame) else { return Vec::new() };
        let mut out = Vec::new();
        if let (Some(d), pc) = self.frame_debug(f) {
            let chain = d.scope_chain(pc);
            let visible: Vec<Option<u32>> = d.visible_variables(pc).map(|v| v.scope).collect();
            let scopes: Vec<(u32, ScopeKind, Pos, Pos)> =
                chain.iter().map(|&s| (s, d.scopes[s as usize].kind, d.scopes[s as usize].start, d.scopes[s as usize].end)).collect();
            let has_upvalues = visible.contains(&None);
            for (s, kind, start, end) in scopes {
                if kind != ScopeKind::Function && !visible.contains(&Some(s)) {
                    continue;
                }
                let name = match kind {
                    ScopeKind::Function => "Local",
                    ScopeKind::Catch => "Catch",
                    ScopeKind::Class => "Class",
                    _ => "Block",
                };
                let r = self.new_ref(RefTarget::Scope { frame, scope: Some(s) });
                out.push(ScopeInfo { name: name.into(), kind: Some(kind), variables_ref: r, range: Some((start, end)) });
            }
            if has_upvalues {
                let r = self.new_ref(RefTarget::Scope { frame, scope: None });
                out.push(ScopeInfo { name: "Closure".into(), kind: None, variables_ref: r, range: None });
            }
        }
        let r = self.new_ref(RefTarget::Global);
        out.push(ScopeInfo { name: "Global".into(), kind: None, variables_ref: r, range: None });
        out
    }

    fn read_binding(&self, f: &Frame, v: &VarDebug) -> Value {
        let vm = &*self.vm;
        match v.location {
            VarLocation::Register(r) => vm.stack[f.base + r as usize].clone(),
            VarLocation::Cell(r) => {
                let x = &vm.stack[f.base + r as usize];
                match x {
                    Value::Object(o) if vm.heap.is_cell(*o) => vm.heap.cell(*o).clone(),
                    other => other.clone(),
                }
            }
            VarLocation::Upvalue(i) => match &vm.heap.obj(f.callee).kind {
                ObjectKind::Closure(c) => c.upvalues.get(i as usize).and_then(|&c| vm.heap.try_cell(c)).cloned().unwrap_or(Value::Undefined),
                _ => Value::Undefined,
            },
        }
    }

    /// Children of a variables reference (scope or object).
    pub fn variables(&mut self, variables_ref: u32) -> Vec<Variable> {
        let Some(&t) = self.refs.get(variables_ref.wrapping_sub(1) as usize) else { return Vec::new() };
        match t {
            RefTarget::Scope { frame, scope } => {
                let f = self.frame(frame).unwrap().clone();
                let bindings: Vec<(String, String, Value)> = self
                    .visible(frame)
                    .into_iter()
                    .filter(|(_, s)| *s == scope)
                    .map(|(v, _)| (v.name.clone(), v.kind.as_str().to_string(), self.read_binding(&f, v)))
                    .collect();
                let mut seen = std::collections::HashSet::new();
                bindings
                    .into_iter()
                    .filter(|(n, _, _)| seen.insert(n.clone()))
                    .map(|(n, k, v)| {
                        let mut var = self.make_variable(n, &v);
                        var.kind = Some(k);
                        var
                    })
                    .collect()
            }
            RefTarget::Global => {
                let g = self.vm.realm.global;
                let keys: Vec<PropKey> =
                    self.vm.own_keys(g).into_iter().filter(|k| self.vm.get_own_property(g, k).is_some_and(|p| p.enumerable())).collect();
                keys.into_iter().map(|k| self.property_variable(g, &k)).collect()
            }
            RefTarget::Object(o) => self.object_children(o),
        }
    }

    fn property_variable(&mut self, o: ObjRef, k: &PropKey) -> Variable {
        let name = k.display();
        match self.vm.get_own_property(o, k) {
            Some(Property { slot: Slot::Data(v), .. }) => self.make_variable(name, &v),
            Some(Property { slot: Slot::Accessor { get, set }, .. }) => Variable {
                name,
                value: match (get.is_some(), set.is_some()) {
                    (true, true) => "[Getter/Setter]",
                    (true, false) => "[Getter]",
                    _ => "[Setter]",
                }
                .into(),
                type_name: "accessor".into(),
                kind: None,
                variables_ref: 0,
            },
            None => self.make_variable(name, &Value::Undefined),
        }
    }

    fn object_children(&mut self, o: ObjRef) -> Vec<Variable> {
        let mut out: Vec<Variable> = Vec::new();
        let keys = self.vm.own_keys(o);
        for k in &keys {
            out.push(self.property_variable(o, k));
        }
        let extra: Vec<(String, Value)> = match &self.vm.heap.obj(o).kind {
            ObjectKind::Map(m) => m
                .entries
                .iter()
                .flatten()
                .map(|(k, v)| (format!("{} =>", crate::inspect::inspect_short(self.vm, k)), v.clone()))
                .collect(),
            ObjectKind::Set(m) => m.entries.iter().flatten().enumerate().map(|(i, (k, _))| (format!("[{i}]"), k.clone())).collect(),
            ObjectKind::Promise(p) => match &p.state {
                PromiseState::Pending => vec![("[[PromiseState]]".into(), Value::str("pending"))],
                PromiseState::Fulfilled(v) => {
                    vec![("[[PromiseState]]".into(), Value::str("fulfilled")), ("[[PromiseResult]]".into(), v.clone())]
                }
                PromiseState::Rejected(v) => {
                    vec![("[[PromiseState]]".into(), Value::str("rejected")), ("[[PromiseResult]]".into(), v.clone())]
                }
            },
            _ => Vec::new(),
        };
        for (n, v) in extra {
            out.push(self.make_variable(n, &v));
        }
        if let Some(p) = self.vm.heap.obj(o).proto {
            out.push(self.make_variable("[[Prototype]]".into(), &Value::Object(p)));
        }
        out
    }

    fn make_variable(&mut self, name: String, v: &Value) -> Variable {
        let (value, type_name) = match v {
            Value::Hole => ("<uninitialized>".to_string(), "uninitialized".to_string()),
            _ => (crate::inspect::inspect_short(self.vm, v), self.vm.type_of(v).to_string()),
        };
        let variables_ref = match v {
            Value::Object(o) => self.new_ref(RefTarget::Object(*o)),
            _ => 0,
        };
        Variable { name, value, type_name, kind: None, variables_ref }
    }

    /// Details of the exception being reported (exception stops only).
    pub fn exception(&mut self) -> Option<ExceptionInfo> {
        let (v, uncaught) = self.vm.debug.current_exception.clone()?;
        let mut stack = None;
        let mut description = crate::inspect::inspect_short(self.vm, &v);
        if let Value::Object(o) = &v {
            if matches!(self.vm.heap.obj(*o).kind, ObjectKind::Error) {
                let read = |k: &str| match self.lookup(&v, &PropKey::str(k)) {
                    Ok(Value::String(s)) => Some(s.to_rust()),
                    _ => None,
                };
                stack = read("stack");
                let name = read("name").unwrap_or_else(|| "Error".into());
                description = match read("message") {
                    Some(m) if !m.is_empty() => format!("{name}: {m}"),
                    _ => name,
                };
            }
        }
        Some(ExceptionInfo { value: self.make_variable("exception".into(), &v), description, stack, uncaught })
    }

    /// Side-effect-free property lookup (own data properties, prototype
    /// chain, string length/indices). Getters are not invoked.
    fn lookup(&self, base: &Value, key: &PropKey) -> Result<Value, String> {
        let vm = &*self.vm;
        let start = match base {
            Value::Object(o) => *o,
            Value::String(s) => {
                if let Some(i) = key.as_index() {
                    return Ok(s
                        .units()
                        .get(i as usize)
                        .map(|&u| Value::String(JsStr::from_units(vec![u])))
                        .unwrap_or(Value::Undefined));
                }
                if matches!(key, PropKey::Str(k) if k.eq_str("length")) {
                    return Ok(Value::Number(s.len() as f64));
                }
                vm.realm.string_proto
            }
            Value::Number(_) => vm.realm.number_proto,
            Value::Bool(_) => vm.realm.boolean_proto,
            Value::Symbol(_) => vm.realm.symbol_proto,
            _ => return Err(format!("Cannot read properties of {} (reading '{}')", crate::inspect::inspect_short(vm, base), key.display())),
        };
        let mut cur = Some(start);
        while let Some(c) = cur {
            match vm.get_own_property(c, key) {
                Some(Property { slot: Slot::Data(v), .. }) => return Ok(v),
                Some(_) => return Err(format!("property '{}' is an accessor (not evaluated)", key.display())),
                None => cur = vm.heap.obj(c).proto,
            }
        }
        Ok(Value::Undefined)
    }

    fn resolve_name(&self, frame: usize, name: &str) -> Result<Value, String> {
        if let Some(f) = self.frame(frame) {
            if name == "this" {
                return Ok(match &f.this {
                    Value::Hole => Value::Undefined,
                    v => v.clone(),
                });
            }
            if let Some((v, _)) = self.visible(frame).into_iter().find(|(v, _)| v.name == name) {
                return Ok(self.read_binding(f, v));
            }
        }
        let key = PropKey::str(name);
        let g = self.vm.realm.global;
        let mut cur = Some(g);
        while let Some(c) = cur {
            if self.vm.get_own_property(c, &key).is_some() {
                return self.lookup(&Value::Object(g), &key);
            }
            cur = self.vm.heap.obj(c).proto;
        }
        Err(format!("{name} is not defined"))
    }

    /// Evaluate a restricted, side-effect-free expression in a frame:
    /// literals (numbers, strings, `true`, `false`, `null`, `undefined`) or a
    /// name / `this` followed by `.prop`, `[index]` or `["key"]` accessors.
    pub fn evaluate(&mut self, frame: usize, expr: &str) -> Result<Variable, String> {
        let v = self.evaluate_value(frame, expr)?;
        if matches!(v, Value::Hole) {
            return Err(format!("Cannot access '{}' before initialization", expr.trim()));
        }
        Ok(self.make_variable(expr.trim().to_string(), &v))
    }

    pub(crate) fn evaluate_value(&self, frame: usize, expr: &str) -> Result<Value, String> {
        let mut p = ExprParser { s: expr.trim().as_bytes(), i: 0 };
        let mut v = match p.primary()? {
            Primary::Value(v) => v,
            Primary::Name(n) => self.resolve_name(frame, &n)?,
        };
        loop {
            p.ws();
            match p.peek() {
                None => return Ok(v),
                Some(b'.') => {
                    p.i += 1;
                    p.ws();
                    let n = p.ident().ok_or("expected property name after '.'")?;
                    v = self.lookup(&v, &PropKey::str(&n))?;
                }
                Some(b'[') => {
                    p.i += 1;
                    p.ws();
                    let key = match p.primary()? {
                        Primary::Value(Value::Number(n)) => PropKey::str(&crate::numconv::number_to_string(n)),
                        Primary::Value(Value::String(s)) => PropKey::Str(s),
                        Primary::Name(n) => match self.resolve_name(frame, &n)? {
                            Value::Number(n) => PropKey::str(&crate::numconv::number_to_string(n)),
                            Value::String(s) => PropKey::Str(s),
                            Value::Symbol(s) => PropKey::Sym(s),
                            _ => return Err("unsupported computed key".into()),
                        },
                        _ => return Err("unsupported computed key".into()),
                    };
                    p.ws();
                    if p.peek() != Some(b']') {
                        return Err("expected ']'".into());
                    }
                    p.i += 1;
                    v = self.lookup(&v, &key)?;
                }
                Some(c) => return Err(format!("unsupported expression syntax at '{}'", c as char)),
            }
        }
    }
}

enum Primary {
    Value(Value),
    Name(String),
}

struct ExprParser<'s> {
    s: &'s [u8],
    i: usize,
}

impl ExprParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn ws(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn ident(&mut self) -> Option<String> {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80) {
            self.i += 1;
        }
        let id = std::str::from_utf8(&self.s[start..self.i]).ok()?;
        if id.is_empty() || id.as_bytes()[0].is_ascii_digit() { None } else { Some(id.to_string()) }
    }

    fn primary(&mut self) -> Result<Primary, String> {
        self.ws();
        match self.peek() {
            Some(q @ (b'"' | b'\'')) => {
                self.i += 1;
                let start = self.i;
                while self.peek().is_some_and(|c| c != q) {
                    self.i += 1;
                }
                if self.peek() != Some(q) {
                    return Err("unterminated string".into());
                }
                let s = std::str::from_utf8(&self.s[start..self.i]).map_err(|e| e.to_string())?.to_string();
                self.i += 1;
                Ok(Primary::Value(Value::str(&s)))
            }
            Some(c) if c.is_ascii_digit() || c == b'-' => {
                let start = self.i;
                self.i += 1;
                while self.peek().is_some_and(|c| c.is_ascii_digit() || c == b'.' || c == b'e' || c == b'E') {
                    self.i += 1;
                }
                let t = std::str::from_utf8(&self.s[start..self.i]).unwrap();
                t.parse::<f64>().map(|n| Primary::Value(Value::Number(n))).map_err(|_| format!("invalid number '{t}'"))
            }
            _ => {
                let id = self.ident().ok_or("expected a name or literal")?;
                Ok(match id.as_str() {
                    "true" => Primary::Value(Value::Bool(true)),
                    "false" => Primary::Value(Value::Bool(false)),
                    "null" => Primary::Value(Value::Null),
                    "undefined" => Primary::Value(Value::Undefined),
                    _ => Primary::Name(id),
                })
            }
        }
    }
}
