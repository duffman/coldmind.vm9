//! The interpreter loop and call machinery.
//!
//! Execution model:
//! * `Vm::frames` is the activation stack; registers of all frames live in
//!   `Vm::stack` (`frame.base .. frame.base + nregs`).
//! * Calls from bytecode to bytecode push a frame and continue in the same
//!   loop (no Rust recursion). Native functions run as Rust calls; when a
//!   native (or an accessor, `super()`, field initializer) calls back into
//!   JavaScript, a nested `run_loop` starts with a *boundary* frame and
//!   returns when that frame completes.
//! * Exceptions unwind frame by frame using each function's handler table;
//!   unwinding stops at the current run loop's boundary frame and surfaces
//!   as `Err(value)` to the Rust caller.
//! * An async function runs synchronously until its first `await`, then its
//!   frame (with registers) moves into a heap `Coroutine`; promise reactions
//!   resume it later from the job queue as a new boundary frame.

use dj_bytecode::{FunctionKind, Instr};

use crate::frame::{Coroutine, Frame, SuspendedFrame};
use crate::heap::{HeapData, ObjRef};
use crate::object::*;
use crate::value::{PropKey, Value};
use crate::vm::{ErrorType, Vm};

/// Call arguments: a window of the caller's registers or an owned vector.
pub(crate) enum Args {
    Regs { start: usize, count: usize },
    Vec(Vec<Value>),
}

impl Args {
    fn len(&self) -> usize {
        match self {
            Args::Regs { count, .. } => *count,
            Args::Vec(v) => v.len(),
        }
    }

    fn get(&self, stack: &[Value], i: usize) -> Value {
        match self {
            Args::Regs { start, count } => {
                if i < *count {
                    stack[start + i].clone()
                } else {
                    Value::Undefined
                }
            }
            Args::Vec(v) => v.get(i).cloned().unwrap_or(Value::Undefined),
        }
    }

    fn into_vec(self, stack: &[Value]) -> Vec<Value> {
        match self {
            Args::Regs { start, count } => stack[start..start + count].to_vec(),
            Args::Vec(v) => v,
        }
    }
}

/// How a frame completion continues.
enum Step {
    /// Resume the (new) top frame.
    Continue,
    /// The run loop's boundary frame finished normally.
    Return(Value),
    /// Throw in the current context (frames above the boundary).
    Throw(Value),
    /// The boundary frame finished abruptly.
    ReturnErr(Value),
}

impl Vm {
    pub(crate) fn push_js_frame(
        &mut self,
        callee: ObjRef,
        this: Value,
        new_target: Value,
        args: Args,
        ret_reg: Option<u32>,
        boundary: bool,
        construct: bool,
    ) -> Result<(), Value> {
        if self.frames.len() >= self.options.max_frames {
            return Err(self.range_error("Maximum call stack size exceeded"));
        }
        let (module, func) = match &self.heap.obj(callee).kind {
            ObjectKind::Closure(c) => (c.module, c.func),
            _ => unreachable!("push_js_frame on non-closure"),
        };
        let lf = self.modules[module as usize].functions[func as usize].clone();
        let n = lf.register_count as usize;
        let np = lf.param_count as usize;
        let argc = args.len();
        let rest = if lf.has_rest {
            let items: Vec<Value> = (np..argc.max(np)).map(|i| args.get(&self.stack, i)).collect();
            Some(self.new_array(items))
        } else {
            None
        };
        let async_promise = if lf.is_async { Some(self.new_promise()) } else { None };
        let base = self.stack.len();
        self.stack.resize(base + n, Value::Undefined);
        for i in 0..np.min(argc) {
            let v = args.get(&self.stack, i);
            self.stack[base + i] = v;
        }
        if let Some(r) = rest {
            self.stack[base + np] = Value::Object(r);
        }
        let id = self.next_frame_id;
        self.next_frame_id += 1;
        self.frames.push(Frame {
            module,
            func,
            callee,
            pc: 0,
            base,
            nregs: n,
            this,
            new_target,
            exception: Value::Undefined,
            ret_reg,
            boundary,
            construct,
            async_promise,
            resumed: false,
            id,
        });
        Ok(())
    }

    fn closure_kind(&self, o: ObjRef) -> Option<FunctionKind> {
        match &self.heap.obj(o).kind {
            ObjectKind::Closure(c) => Some(self.modules[c.module as usize].functions[c.func as usize].kind),
            _ => None,
        }
    }

    pub(crate) fn call_native(&mut self, callee: ObjRef, this: Value, args: Vec<Value>, new_target: Value) -> Result<Value, Value> {
        if self.native_stack.len() >= self.options.max_native_depth {
            return Err(self.range_error("Maximum call stack size exceeded"));
        }
        let (f, name) = match &self.heap.obj(callee).kind {
            ObjectKind::Native(n) => (n.f, n.name.to_rust()),
            _ => unreachable!(),
        };
        let mark = self.roots.len();
        self.roots.push(Value::Object(callee));
        self.roots.push(this.clone());
        self.roots.push(new_target.clone());
        self.roots.extend(args.iter().cloned());
        self.native_stack.push(crate::frame::NativeFrame { name, depth: self.frames.len() });
        let call = NativeCall { this, args, new_target, callee };
        let r = f(self, &call);
        self.native_stack.pop();
        self.roots.truncate(mark);
        r
    }

    fn not_callable(&mut self, v: &Value) -> Value {
        let s = crate::inspect::inspect_short(self, v);
        self.type_error(format!("{s} is not a function"))
    }

    /// `Call` from bytecode. Returns `true` when a frame was pushed.
    fn call_from_frame(&mut self, callee: Value, this: Value, args: Args, dst: u32) -> Result<bool, Value> {
        let Value::Object(o) = callee else { return Err(self.not_callable(&callee)) };
        match self.closure_kind(o) {
            Some(k @ (FunctionKind::ClassConstructor | FunctionKind::DerivedConstructor)) => {
                let _ = k;
                let name = self.function_name(o);
                Err(self.type_error(format!("Class constructor {name} cannot be invoked without 'new'")))
            }
            Some(_) => {
                self.push_js_frame(o, this, Value::Undefined, args, Some(dst), false, false)?;
                Ok(true)
            }
            None => {
                if !matches!(self.heap.obj(o).kind, ObjectKind::Native(_)) {
                    return Err(self.not_callable(&callee));
                }
                let argv = args.into_vec(&self.stack);
                let v = self.call_native(o, this, argv, Value::Undefined)?;
                let base = self.frames.last().unwrap().base;
                self.stack[base + dst as usize] = v;
                Ok(false)
            }
        }
    }

    pub fn function_name(&self, o: ObjRef) -> String {
        match self.heap.obj(o).props.get(&PropKey::str("name")) {
            Some(Property { slot: Slot::Data(Value::String(s)), .. }) if !s.is_empty() => s.to_rust(),
            _ => "anonymous".into(),
        }
    }

    pub fn is_constructor(&self, v: &Value) -> bool {
        let Value::Object(o) = v else { return false };
        match &self.heap.obj(*o).kind {
            ObjectKind::Closure(c) => self.modules[c.module as usize].functions[c.func as usize].kind.is_constructor()
                && !self.modules[c.module as usize].functions[c.func as usize].is_async,
            ObjectKind::Native(n) => n.constructor,
            _ => false,
        }
    }

    /// OrdinaryCreateFromConstructor: `this` for a base constructor.
    fn construct_this(&mut self, new_target: &Value) -> Result<Value, Value> {
        let proto = self.get(new_target, &PropKey::str("prototype"))?;
        let proto = match proto {
            Value::Object(p) => p,
            _ => self.realm.object_proto,
        };
        let o = self.new_object_with(Some(proto), ObjectKind::Ordinary);
        Ok(Value::Object(o))
    }

    fn not_constructor(&mut self, v: &Value) -> Value {
        let s = crate::inspect::inspect_short(self, v);
        self.type_error(format!("{s} is not a constructor"))
    }

    /// `new` from bytecode. Returns `true` when a frame was pushed.
    fn construct_from_frame(&mut self, callee: Value, args: Args, new_target: Value, dst: u32) -> Result<bool, Value> {
        if !self.is_constructor(&callee) {
            return Err(self.not_constructor(&callee));
        }
        let o = callee.as_object().unwrap();
        match self.closure_kind(o) {
            Some(k) => {
                let this = if k == FunctionKind::DerivedConstructor { Value::Hole } else { self.construct_this(&new_target)? };
                self.push_js_frame(o, this, new_target, args, Some(dst), false, true)?;
                Ok(true)
            }
            None => {
                let argv = args.into_vec(&self.stack);
                let v = self.call_native(o, Value::Undefined, argv, new_target)?;
                let base = self.frames.last().unwrap().base;
                self.stack[base + dst as usize] = v;
                Ok(false)
            }
        }
    }

    /// Call `f` synchronously (nested run loop for bytecode functions).
    pub fn call(&mut self, f: &Value, this: Value, args: Vec<Value>) -> Result<Value, Value> {
        let Value::Object(o) = f else { return Err(self.not_callable(f)) };
        match self.closure_kind(*o) {
            Some(FunctionKind::ClassConstructor | FunctionKind::DerivedConstructor) => {
                let name = self.function_name(*o);
                Err(self.type_error(format!("Class constructor {name} cannot be invoked without 'new'")))
            }
            Some(_) => {
                self.push_js_frame(*o, this, Value::Undefined, Args::Vec(args), None, true, false)?;
                self.run_loop(None)
            }
            None => match self.heap.obj(*o).kind {
                ObjectKind::Native(_) => self.call_native(*o, this, args, Value::Undefined),
                _ => Err(self.not_callable(f)),
            },
        }
    }

    /// `new f(...args)` synchronously. `new_target` defaults to `f`.
    pub fn construct(&mut self, f: &Value, args: Vec<Value>, new_target: Option<Value>) -> Result<Value, Value> {
        if !self.is_constructor(f) {
            return Err(self.not_constructor(f));
        }
        let o = f.as_object().unwrap();
        let nt = new_target.unwrap_or_else(|| f.clone());
        match self.closure_kind(o) {
            Some(k) => {
                let mark = self.push_root(nt.clone());
                let this = if k == FunctionKind::DerivedConstructor { Value::Hole } else { self.construct_this(&nt)? };
                self.pop_roots(mark);
                self.push_js_frame(o, this, nt, Args::Vec(args), None, true, true)?;
                self.run_loop(None)
            }
            None => self.call_native(o, Value::Undefined, args, nt),
        }
    }

    pub fn is_callable(&self, v: &Value) -> bool {
        match v {
            Value::Object(o) => self.heap.obj(*o).is_callable(),
            _ => false,
        }
    }

    /// `super(...args)` inside the derived constructor running in frame `fi`.
    fn super_call(&mut self, fi: usize, args: Vec<Value>) -> Result<Value, Value> {
        let callee = self.frames[fi].callee;
        let parent = self.heap.obj(callee).proto.map(Value::Object).unwrap_or(Value::Null);
        if !self.is_constructor(&parent) {
            return Err(self.type_error("Super constructor is not a constructor"));
        }
        let nt = self.frames[fi].new_target.clone();
        let result = self.construct(&parent, args, Some(nt))?;
        if !matches!(self.frames[fi].this, Value::Hole) {
            return Err(self.reference_error("Super constructor may only be called once"));
        }
        self.frames[fi].this = result.clone();
        self.run_field_init(callee, &result)?;
        Ok(result)
    }

    fn run_field_init(&mut self, ctor: ObjRef, this: &Value) -> Result<(), Value> {
        let init = match &self.heap.obj(ctor).kind {
            ObjectKind::Closure(c) => c.fields_init,
            _ => None,
        };
        if let Some(init) = init {
            self.call(&Value::Object(init), this.clone(), Vec::new())?;
        }
        Ok(())
    }

    fn home_object(&self, fi: usize) -> Option<ObjRef> {
        match &self.heap.obj(self.frames[fi].callee).kind {
            ObjectKind::Closure(c) => c.home_object,
            _ => None,
        }
    }

    /// Pop the top frame after `Ret`.
    fn complete(&mut self, v: Value) -> Step {
        let frame = self.frames.pop().expect("frame");
        self.stack.truncate(frame.base);
        self.debug_on_return(&frame);
        let mut v = v;
        let mut err = None;
        if frame.construct && !matches!(v, Value::Object(_)) {
            let derived = self.modules[frame.module as usize].functions[frame.func as usize].kind == FunctionKind::DerivedConstructor;
            if derived && !v.is_undefined() {
                err = Some(self.type_error("Derived constructors may only return object or undefined"));
            } else if matches!(frame.this, Value::Hole) {
                err = Some(self.reference_error(
                    "Must call super constructor in derived class before accessing 'this' or returning from derived constructor",
                ));
            } else {
                v = frame.this.clone();
            }
        }
        if let Some(e) = err {
            return if frame.boundary { Step::ReturnErr(e) } else { Step::Throw(e) };
        }
        if let Some(p) = frame.async_promise {
            self.resolve_promise(p, v);
            v = if frame.resumed { Value::Undefined } else { Value::Object(p) };
        }
        if frame.boundary {
            return Step::Return(v);
        }
        if let Some(r) = frame.ret_reg {
            let base = self.frames.last().expect("caller").base;
            self.stack[base + r as usize] = v;
        }
        Step::Continue
    }

    /// Unwind `exc` from the top frame.
    fn unwind(&mut self, exc: Value) -> Step {
        self.debug_on_throw(&exc);
        loop {
            let fi = self.frames.len() - 1;
            if !self.terminating {
                let f = &self.frames[fi];
                let lf = &self.modules[f.module as usize].functions[f.func as usize];
                if let Some(target) = lf.handler_for(f.pc.saturating_sub(1)) {
                    let f = &mut self.frames[fi];
                    f.exception = exc;
                    f.pc = target;
                    return Step::Continue;
                }
            }
            let frame = self.frames.pop().unwrap();
            self.stack.truncate(frame.base);
            self.debug_on_return(&frame);
            if let (Some(p), false) = (frame.async_promise, self.terminating) {
                self.reject_promise(p, exc);
                let v = if frame.resumed { Value::Undefined } else { Value::Object(p) };
                if frame.boundary {
                    return Step::Return(v);
                }
                if let Some(r) = frame.ret_reg {
                    let base = self.frames.last().unwrap().base;
                    self.stack[base + r as usize] = v;
                }
                return Step::Continue;
            }
            if frame.boundary {
                return Step::ReturnErr(exc);
            }
        }
    }

    /// Resume a suspended async activation with the settled value.
    pub(crate) fn resume_coroutine(&mut self, coro: ObjRef, result: Result<Value, Value>) -> Result<(), Value> {
        let st = match self.heap.get_mut(coro) {
            HeapData::Coroutine(c) => c.state.take(),
            _ => None,
        };
        let Some(st) = st else { return Ok(()) };
        let base = self.stack.len();
        let mut frame = st.frame;
        frame.base = base;
        frame.boundary = true;
        frame.ret_reg = None;
        frame.resumed = true;
        self.stack.extend(st.regs);
        self.frames.push(frame);
        let r = match result {
            Ok(v) => {
                self.stack[base + st.await_dst as usize] = v;
                self.run_loop(None)
            }
            Err(e) => self.run_loop(Some(e)),
        };
        r.map(|_| ())
    }

    /// Execute until the current boundary frame completes.
    pub(crate) fn run_loop(&mut self, initial_throw: Option<Value>) -> Result<Value, Value> {
        if self.native_stack.len() + 1 >= self.options.max_native_depth {
            // The boundary frame was pushed by our caller; drop it.
            let f = self.frames.pop().unwrap();
            self.stack.truncate(f.base);
            return Err(self.range_error("Maximum call stack size exceeded"));
        }
        macro_rules! dispatch_step {
            ($step:expr) => {
                match $step {
                    Step::Continue => {}
                    Step::Return(v) => return Ok(v),
                    Step::ReturnErr(e) => return Err(e),
                    Step::Throw(e) => match self.unwind(e) {
                        Step::Continue => {}
                        Step::Return(v) => return Ok(v),
                        Step::ReturnErr(e) => return Err(e),
                        Step::Throw(_) => unreachable!(),
                    },
                }
            };
        }
        if let Some(e) = initial_throw {
            dispatch_step!(self.unwind(e));
        }
        'frames: loop {
            let fi = self.frames.len() - 1;
            let (mi, fidx, base) = {
                let f = &self.frames[fi];
                (f.module as usize, f.func as usize, f.base)
            };
            let module = self.modules[mi].clone();
            let lf = module.functions[fidx].clone();

            macro_rules! r {
                ($x:expr) => {
                    self.stack[base + $x as usize]
                };
            }
            macro_rules! throw {
                ($e:expr) => {{
                    let e = $e;
                    dispatch_step!(self.unwind(e));
                    continue 'frames;
                }};
            }
            macro_rules! tri {
                ($e:expr) => {
                    match $e {
                        Ok(v) => v,
                        Err(e) => throw!(e),
                    }
                };
            }
            macro_rules! name {
                ($k:expr) => {
                    module.name($k)
                };
            }
            macro_rules! num_binop {
                ($dst:expr, $l:expr, $rr:expr, |$a:ident, $b:ident| $e:expr) => {{
                    let v = match (&r!($l), &r!($rr)) {
                        (Value::Number($a), Value::Number($b)) => {
                            let ($a, $b) = (*$a, *$b);
                            $e
                        }
                        _ => {
                            let (x, y) = (r!($l).clone(), r!($rr).clone());
                            let $a = tri!(self.to_number(&x));
                            let $b = tri!(self.to_number(&y));
                            $e
                        }
                    };
                    r!($dst) = Value::Number(v);
                }};
            }

            loop {
                if self.heap.should_collect() {
                    self.collect_garbage();
                }
                let pc = self.frames[fi].pc;
                if self.debug.active && self.debug_safepoint(fi, &lf, pc) {
                    self.terminating = true;
                    throw!(Value::Undefined);
                }
                let ins = lf.code[pc];
                self.frames[fi].pc = pc + 1;
                match ins {
                    Instr::Nop {} => {}
                    Instr::Mov { dst, src } => {
                        let v = r!(src).clone();
                        r!(dst) = v;
                    }
                    Instr::LoadConst { dst, k } => r!(dst) = module.constants[k as usize].clone(),
                    Instr::LoadInt { dst, value } => r!(dst) = Value::Number(value as i32 as f64),
                    Instr::LoadUndef { dst } => r!(dst) = Value::Undefined,
                    Instr::LoadNull { dst } => r!(dst) = Value::Null,
                    Instr::LoadTrue { dst } => r!(dst) = Value::Bool(true),
                    Instr::LoadFalse { dst } => r!(dst) = Value::Bool(false),
                    Instr::LoadHole { dst } => r!(dst) = Value::Hole,
                    Instr::CheckTdz { reg, name } => {
                        if matches!(r!(reg), Value::Hole) {
                            throw!(self.reference_error(format!("Cannot access '{}' before initialization", name!(name))));
                        }
                    }
                    Instr::Box { reg } => {
                        let v = std::mem::take(&mut r!(reg));
                        let c = self.alloc_cell(v);
                        r!(reg) = Value::Object(c);
                    }
                    Instr::NewCell { dst } => {
                        let c = self.alloc_cell(Value::Hole);
                        r!(dst) = Value::Object(c);
                    }
                    Instr::CellGet { dst, cell } => {
                        let c = r!(cell).as_object().expect("cell");
                        let v = self.heap.cell(c).clone();
                        r!(dst) = v;
                    }
                    Instr::CellGetChecked { dst, cell, name } => {
                        let c = r!(cell).as_object().expect("cell");
                        let v = self.heap.cell(c).clone();
                        if matches!(v, Value::Hole) {
                            throw!(self.reference_error(format!("Cannot access '{}' before initialization", name!(name))));
                        }
                        r!(dst) = v;
                    }
                    Instr::CellSet { cell, src } => {
                        let c = r!(cell).as_object().expect("cell");
                        let v = r!(src).clone();
                        *self.heap.cell_mut(c) = v;
                    }
                    Instr::CellCopy { cell } => {
                        let c = r!(cell).as_object().expect("cell");
                        let v = self.heap.cell(c).clone();
                        let n = self.alloc_cell(v);
                        r!(cell) = Value::Object(n);
                    }
                    Instr::UpvalGet { dst, idx } | Instr::UpvalGetChecked { dst, idx, .. } => {
                        let callee = self.frames[fi].callee;
                        let c = match &self.heap.obj(callee).kind {
                            ObjectKind::Closure(cl) => cl.upvalues[idx as usize],
                            _ => unreachable!(),
                        };
                        let v = self.heap.cell(c).clone();
                        if let (Value::Hole, Instr::UpvalGetChecked { name, .. }) = (&v, ins) {
                            throw!(self.reference_error(format!("Cannot access '{}' before initialization", name!(name))));
                        }
                        r!(dst) = v;
                    }
                    Instr::UpvalSet { idx, src } => {
                        let callee = self.frames[fi].callee;
                        let c = match &self.heap.obj(callee).kind {
                            ObjectKind::Closure(cl) => cl.upvalues[idx as usize],
                            _ => unreachable!(),
                        };
                        let v = r!(src).clone();
                        *self.heap.cell_mut(c) = v;
                    }
                    Instr::GetGlobal { dst, name } => {
                        let key = PropKey::Str(name!(name));
                        let g = Value::Object(self.realm.global);
                        if !tri!(self.has_property(self.realm.global, &key)) {
                            throw!(self.reference_error(format!("{} is not defined", key.display())));
                        }
                        let v = tri!(self.get(&g, &key));
                        r!(dst) = v;
                    }
                    Instr::SetGlobal { name, src } => {
                        let key = PropKey::Str(name!(name));
                        if !tri!(self.has_property(self.realm.global, &key)) {
                            throw!(self.reference_error(format!("{} is not defined", key.display())));
                        }
                        let v = r!(src).clone();
                        tri!(self.set(&Value::Object(self.realm.global), key, v));
                    }
                    Instr::TypeofGlobal { dst, name } => {
                        let key = PropKey::Str(name!(name));
                        let s = if tri!(self.has_property(self.realm.global, &key)) {
                            let v = tri!(self.get(&Value::Object(self.realm.global), &key));
                            self.type_of(&v)
                        } else {
                            "undefined"
                        };
                        r!(dst) = Value::str(s);
                    }
                    Instr::LoadThis { dst } => {
                        let v = self.frames[fi].this.clone();
                        r!(dst) = v;
                    }
                    Instr::LoadNewTarget { dst } => {
                        let v = self.frames[fi].new_target.clone();
                        r!(dst) = v;
                    }
                    Instr::LoadCallee { dst } => r!(dst) = Value::Object(self.frames[fi].callee),

                    Instr::Add { dst, lhs, rhs } => {
                        let v = match (&r!(lhs), &r!(rhs)) {
                            (Value::Number(a), Value::Number(b)) => Value::Number(a + b),
                            (Value::String(a), Value::String(b)) => Value::String(a.concat(b)),
                            _ => {
                                let (a, b) = (r!(lhs).clone(), r!(rhs).clone());
                                tri!(self.op_add(&a, &b))
                            }
                        };
                        r!(dst) = v;
                    }
                    Instr::Sub { dst, lhs, rhs } => num_binop!(dst, lhs, rhs, |a, b| a - b),
                    Instr::Mul { dst, lhs, rhs } => num_binop!(dst, lhs, rhs, |a, b| a * b),
                    Instr::Div { dst, lhs, rhs } => num_binop!(dst, lhs, rhs, |a, b| a / b),
                    Instr::Mod { dst, lhs, rhs } => num_binop!(dst, lhs, rhs, |a, b| a % b),
                    Instr::Exp { dst, lhs, rhs } => num_binop!(dst, lhs, rhs, |a, b| crate::numconv::js_pow(a, b)),
                    Instr::Shl { dst, lhs, rhs } => num_binop!(dst, lhs, rhs, |a, b| crate::numconv::to_int32(a)
                        .wrapping_shl(crate::numconv::to_uint32(b) & 31)
                        as f64),
                    Instr::Shr { dst, lhs, rhs } => {
                        num_binop!(dst, lhs, rhs, |a, b| (crate::numconv::to_int32(a) >> (crate::numconv::to_uint32(b) & 31)) as f64)
                    }
                    Instr::UShr { dst, lhs, rhs } => {
                        num_binop!(dst, lhs, rhs, |a, b| (crate::numconv::to_uint32(a) >> (crate::numconv::to_uint32(b) & 31)) as f64)
                    }
                    Instr::BitAnd { dst, lhs, rhs } => {
                        num_binop!(dst, lhs, rhs, |a, b| (crate::numconv::to_int32(a) & crate::numconv::to_int32(b)) as f64)
                    }
                    Instr::BitOr { dst, lhs, rhs } => {
                        num_binop!(dst, lhs, rhs, |a, b| (crate::numconv::to_int32(a) | crate::numconv::to_int32(b)) as f64)
                    }
                    Instr::BitXor { dst, lhs, rhs } => {
                        num_binop!(dst, lhs, rhs, |a, b| (crate::numconv::to_int32(a) ^ crate::numconv::to_int32(b)) as f64)
                    }
                    Instr::Eq { dst, lhs, rhs } | Instr::Ne { dst, lhs, rhs } => {
                        let (a, b) = (r!(lhs).clone(), r!(rhs).clone());
                        let eq = tri!(self.loose_equals(&a, &b));
                        r!(dst) = Value::Bool(if matches!(ins, Instr::Eq { .. }) { eq } else { !eq });
                    }
                    Instr::StrictEq { dst, lhs, rhs } => {
                        let v = r!(lhs).strict_equals(&r!(rhs));
                        r!(dst) = Value::Bool(v);
                    }
                    Instr::StrictNe { dst, lhs, rhs } => {
                        let v = !r!(lhs).strict_equals(&r!(rhs));
                        r!(dst) = Value::Bool(v);
                    }
                    Instr::Lt { dst, lhs, rhs } | Instr::Gt { dst, lhs, rhs } | Instr::Le { dst, lhs, rhs } | Instr::Ge { dst, lhs, rhs } => {
                        let v = match (&r!(lhs), &r!(rhs)) {
                            (Value::Number(a), Value::Number(b)) => match ins {
                                Instr::Lt { .. } => a < b,
                                Instr::Gt { .. } => a > b,
                                Instr::Le { .. } => a <= b,
                                _ => a >= b,
                            },
                            _ => {
                                let (a, b) = (r!(lhs).clone(), r!(rhs).clone());
                                match ins {
                                    Instr::Lt { .. } => tri!(self.abstract_lt(&a, &b, true)) == Some(true),
                                    Instr::Gt { .. } => tri!(self.abstract_lt(&b, &a, false)) == Some(true),
                                    Instr::Le { .. } => tri!(self.abstract_lt(&b, &a, false)) == Some(false),
                                    _ => tri!(self.abstract_lt(&a, &b, true)) == Some(false),
                                }
                            }
                        };
                        r!(dst) = Value::Bool(v);
                    }
                    Instr::InstanceOf { dst, lhs, rhs } => {
                        let (a, b) = (r!(lhs).clone(), r!(rhs).clone());
                        let v = tri!(self.instance_of(&a, &b));
                        r!(dst) = Value::Bool(v);
                    }
                    Instr::In { dst, lhs, rhs } => {
                        let (a, b) = (r!(lhs).clone(), r!(rhs).clone());
                        let Value::Object(o) = b else {
                            let s = crate::inspect::inspect_short(self, &b);
                            throw!(self.type_error(format!("Cannot use 'in' operator to search for a key in {s}")));
                        };
                        let k = tri!(self.to_property_key(&a));
                        let v = tri!(self.has_property(o, &k));
                        r!(dst) = Value::Bool(v);
                    }

                    Instr::Neg { dst, src } => {
                        let x = r!(src).clone();
                        let n = tri!(self.to_number(&x));
                        r!(dst) = Value::Number(-n);
                    }
                    Instr::ToNumber { dst, src } => {
                        let x = r!(src).clone();
                        let n = tri!(self.to_number(&x));
                        r!(dst) = Value::Number(n);
                    }
                    Instr::Inc { dst, src } | Instr::Dec { dst, src } => {
                        let n = match &r!(src) {
                            Value::Number(n) => *n,
                            _ => {
                                let x = r!(src).clone();
                                tri!(self.to_number(&x))
                            }
                        };
                        r!(dst) = Value::Number(if matches!(ins, Instr::Inc { .. }) { n + 1.0 } else { n - 1.0 });
                    }
                    Instr::BitNot { dst, src } => {
                        let x = r!(src).clone();
                        let n = tri!(self.to_number(&x));
                        r!(dst) = Value::Number(!crate::numconv::to_int32(n) as f64);
                    }
                    Instr::Not { dst, src } => {
                        let v = !r!(src).truthy();
                        r!(dst) = Value::Bool(v);
                    }
                    Instr::TypeOf { dst, src } => {
                        let s = self.type_of(&r!(src));
                        r!(dst) = Value::str(s);
                    }
                    Instr::ToStr { dst, src } => {
                        let x = r!(src).clone();
                        let s = tri!(self.to_string(&x));
                        r!(dst) = Value::String(s);
                    }
                    Instr::ToPropKey { dst, src } => {
                        let x = r!(src).clone();
                        let k = tri!(self.to_property_key(&x));
                        r!(dst) = k.to_value();
                    }

                    Instr::GetProp { dst, obj, key } => {
                        // Fast path: array element by integer index.
                        if let (Value::Object(o), Value::Number(n)) = (&r!(obj), &r!(key)) {
                            let (o, n) = (*o, *n);
                            if let ObjectKind::Array(els) = &self.heap.obj(o).kind {
                                if n >= 0.0 && n.fract() == 0.0 && (n as usize) < els.len() {
                                    let v = els[n as usize].clone();
                                    if !matches!(v, Value::Hole) {
                                        r!(dst) = v;
                                        continue;
                                    }
                                }
                            }
                        }
                        let (o, k) = (r!(obj).clone(), r!(key).clone());
                        let k = tri!(self.to_property_key_for(&o, &k));
                        let v = tri!(self.get(&o, &k));
                        r!(dst) = v;
                    }
                    Instr::GetNamed { dst, obj, name } => {
                        let o = r!(obj).clone();
                        let v = tri!(self.get(&o, &PropKey::Str(name!(name))));
                        r!(dst) = v;
                    }
                    Instr::SetProp { obj, key, value } => {
                        if let (Value::Object(o), Value::Number(n)) = (&r!(obj), &r!(key)) {
                            let (o, n) = (*o, *n);
                            let v = r!(value).clone();
                            let ob = self.heap.obj_mut(o);
                            let frozen = ob.elements_frozen;
                            if let ObjectKind::Array(els) = &mut ob.kind {
                                if !frozen && n >= 0.0 && n.fract() == 0.0 && (n as usize) < els.len() && !matches!(els[n as usize], Value::Hole) {
                                    els[n as usize] = v;
                                    continue;
                                }
                            }
                        }
                        let (o, k, v) = (r!(obj).clone(), r!(key).clone(), r!(value).clone());
                        let k = tri!(self.to_property_key_for(&o, &k));
                        tri!(self.set(&o, k, v));
                    }
                    Instr::SetNamed { obj, name, value } => {
                        let (o, v) = (r!(obj).clone(), r!(value).clone());
                        tri!(self.set(&o, PropKey::Str(name!(name)), v));
                    }
                    Instr::DeleteProp { dst, obj, key } => {
                        let (o, k) = (r!(obj).clone(), r!(key).clone());
                        let k = tri!(self.to_property_key_for(&o, &k));
                        let v = tri!(self.delete_property_value(&o, &k));
                        r!(dst) = Value::Bool(v);
                    }
                    Instr::DefineField { obj, key, value } => {
                        let (o, k, v) = (r!(obj).clone(), r!(key).clone(), r!(value).clone());
                        let k = tri!(self.to_property_key(&k));
                        let o = o.as_object().expect("DefineField on object");
                        tri!(self.create_data_property(o, k, v));
                    }
                    Instr::DefineNamed { obj, name, value } => {
                        let (o, v) = (r!(obj).clone(), r!(value).clone());
                        let o = o.as_object().expect("DefineNamed on object");
                        tri!(self.create_data_property(o, PropKey::Str(name!(name)), v));
                    }
                    Instr::DefineMethod { obj, key, func, flags } => {
                        let (o, k, f) = (r!(obj).clone(), r!(key).clone(), r!(func).clone());
                        let k = tri!(self.to_property_key(&k));
                        tri!(self.define_method_property(o.as_object().expect("object"), k, f.as_object().expect("function"), flags));
                    }
                    Instr::CopyDataProps { target, source, excl, count } => {
                        let (t, s) = (r!(target).clone(), r!(source).clone());
                        let mut excluded = Vec::with_capacity(count as usize);
                        for i in 0..count {
                            let v = r!(excl + i).clone();
                            excluded.push(tri!(self.to_property_key(&v)));
                        }
                        tri!(self.copy_data_properties(t.as_object().expect("object"), &s, &excluded));
                    }
                    Instr::NewObject { dst } => {
                        let o = self.new_object();
                        r!(dst) = Value::Object(o);
                    }
                    Instr::NewArray { dst } => {
                        let o = self.new_array(Vec::new());
                        r!(dst) = Value::Object(o);
                    }
                    Instr::ArrayPush { array, value } => {
                        let a = r!(array).as_object().expect("array");
                        let v = r!(value).clone();
                        if let ObjectKind::Array(els) = &mut self.heap.obj_mut(a).kind {
                            els.push(v);
                        }
                    }
                    Instr::ArrayHole { array } => {
                        let a = r!(array).as_object().expect("array");
                        if let ObjectKind::Array(els) = &mut self.heap.obj_mut(a).kind {
                            els.push(Value::Hole);
                        }
                    }
                    Instr::ArraySpread { array, iterable } => {
                        let a = r!(array).as_object().expect("array");
                        let it = r!(iterable).clone();
                        let items = tri!(self.iterate_to_vec(&it));
                        if let ObjectKind::Array(els) = &mut self.heap.obj_mut(a).kind {
                            els.extend(items);
                        }
                    }

                    Instr::Closure { dst, func } => {
                        let child = module.functions[func as usize].clone();
                        let callee = self.frames[fi].callee;
                        let mut ups = Vec::with_capacity(child.upvalues.len());
                        for d in &child.upvalues {
                            let c = if d.from_parent_register {
                                r!(d.index).as_object().expect("captured register holds a cell")
                            } else {
                                match &self.heap.obj(callee).kind {
                                    ObjectKind::Closure(cl) => cl.upvalues[d.index as usize],
                                    _ => unreachable!(),
                                }
                            };
                            ups.push(c);
                        }
                        let f = self.make_closure(mi as u32, func, ups.into_boxed_slice());
                        r!(dst) = Value::Object(f);
                    }
                    Instr::MakeClass { dst, ctor, parent, has_parent } => {
                        let c = r!(ctor).as_object().expect("class ctor closure");
                        let p = if has_parent == 1 { Some(r!(parent).clone()) } else { None };
                        tri!(self.make_class(c, p));
                        r!(dst) = Value::Object(c);
                    }
                    Instr::SetFieldInit { class, init } => {
                        let c = r!(class).as_object().expect("class");
                        let i = r!(init).as_object();
                        if let ObjectKind::Closure(cl) = &mut self.heap.obj_mut(c).kind {
                            cl.fields_init = i;
                        }
                    }
                    Instr::InitFields {} => {
                        let callee = self.frames[fi].callee;
                        let this = self.frames[fi].this.clone();
                        tri!(self.run_field_init(callee, &this));
                    }
                    Instr::Call { dst, callee, this, args, argc } => {
                        let (f, t) = (r!(callee).clone(), r!(this).clone());
                        if tri!(self.call_from_frame(f, t, Args::Regs { start: base + args as usize, count: argc as usize }, dst)) {
                            continue 'frames;
                        }
                    }
                    Instr::CallFn { dst, callee, args, argc } => {
                        let f = r!(callee).clone();
                        if tri!(self.call_from_frame(f, Value::Undefined, Args::Regs { start: base + args as usize, count: argc as usize }, dst)) {
                            continue 'frames;
                        }
                    }
                    Instr::CallSpread { dst, callee, this, array } => {
                        let (f, t) = (r!(callee).clone(), r!(this).clone());
                        let argv = self.array_elements(r!(array).as_object().expect("array"));
                        if tri!(self.call_from_frame(f, t, Args::Vec(argv), dst)) {
                            continue 'frames;
                        }
                    }
                    Instr::CallFnSpread { dst, callee, array } => {
                        let (f, t) = (r!(callee).clone(), Value::Undefined);
                        let argv = self.array_elements(r!(array).as_object().expect("array"));
                        if tri!(self.call_from_frame(f, t, Args::Vec(argv), dst)) {
                            continue 'frames;
                        }
                    }
                    Instr::New { dst, callee, args, argc } => {
                        let f = r!(callee).clone();
                        if tri!(self.construct_from_frame(f.clone(), Args::Regs { start: base + args as usize, count: argc as usize }, f, dst)) {
                            continue 'frames;
                        }
                    }
                    Instr::NewSpread { dst, callee, array } => {
                        let f = r!(callee).clone();
                        let argv = self.array_elements(r!(array).as_object().expect("array"));
                        if tri!(self.construct_from_frame(f.clone(), Args::Vec(argv), f, dst)) {
                            continue 'frames;
                        }
                    }
                    Instr::SuperCall { dst, args, argc } => {
                        let argv: Vec<Value> = (0..argc).map(|i| r!(args + i).clone()).collect();
                        let v = tri!(self.super_call(fi, argv));
                        r!(dst) = v;
                    }
                    Instr::SuperCallSpread { dst, array } => {
                        let argv = self.array_elements(r!(array).as_object().expect("array"));
                        let v = tri!(self.super_call(fi, argv));
                        r!(dst) = v;
                    }
                    Instr::SuperGet { dst, key } => {
                        let k = r!(key).clone();
                        let k = tri!(self.to_property_key(&k));
                        let Some(home) = self.home_object(fi) else { throw!(self.syntax_error("'super' keyword unexpected here")) };
                        let this = self.frames[fi].this.clone();
                        if matches!(this, Value::Hole) {
                            throw!(self.reference_error("Must call super constructor in derived class before accessing 'this'"));
                        }
                        let v = match self.heap.obj(home).proto {
                            Some(p) => tri!(self.get_with_receiver(p, &k, &this)),
                            None => Value::Undefined,
                        };
                        r!(dst) = v;
                    }
                    Instr::SuperSet { key, value } => {
                        let (k, v) = (r!(key).clone(), r!(value).clone());
                        let k = tri!(self.to_property_key(&k));
                        let Some(home) = self.home_object(fi) else { throw!(self.syntax_error("'super' keyword unexpected here")) };
                        let this = self.frames[fi].this.clone();
                        match self.heap.obj(home).proto {
                            Some(p) => tri!(self.set_with_receiver(p, k, v, &this)),
                            None => throw!(self.type_error("Cannot set property on null prototype")),
                        }
                    }

                    Instr::GetIter { dst, iterable } => {
                        let v = r!(iterable).clone();
                        let it = tri!(self.get_iterator(&v));
                        r!(dst) = Value::Object(it);
                    }
                    Instr::IterNext { done, value, iter } => {
                        let it = r!(iter).as_object().expect("iterator");
                        match tri!(self.iterator_step(it)) {
                            Some(v) => {
                                r!(done) = Value::Bool(false);
                                r!(value) = v;
                            }
                            None => {
                                r!(done) = Value::Bool(true);
                                r!(value) = Value::Undefined;
                            }
                        }
                    }
                    Instr::IterClose { iter } => {
                        let it = r!(iter).as_object().expect("iterator");
                        tri!(self.iterator_close(it));
                    }
                    Instr::ForInIter { dst, obj } => {
                        let o = r!(obj).clone();
                        let it = tri!(self.for_in_iterator(&o));
                        r!(dst) = Value::Object(it);
                    }

                    Instr::Jmp { target } => self.frames[fi].pc = target as usize,
                    Instr::JmpTrue { cond, target } => {
                        if r!(cond).truthy() {
                            self.frames[fi].pc = target as usize;
                        }
                    }
                    Instr::JmpFalse { cond, target } => {
                        if !r!(cond).truthy() {
                            self.frames[fi].pc = target as usize;
                        }
                    }
                    Instr::Ret { src } => {
                        let v = r!(src).clone();
                        dispatch_step!(self.complete(v));
                        continue 'frames;
                    }
                    Instr::Throw { src } => {
                        let v = r!(src).clone();
                        throw!(v);
                    }
                    Instr::ThrowError { kind, msg } => {
                        let t = match kind {
                            0 => ErrorType::TypeError,
                            1 => ErrorType::ReferenceError,
                            2 => ErrorType::SyntaxError,
                            _ => ErrorType::RangeError,
                        };
                        let m = name!(msg).to_rust();
                        throw!(self.make_error(t, &m));
                    }
                    Instr::GetException { dst } => {
                        let e = std::mem::take(&mut self.frames[fi].exception);
                        r!(dst) = e;
                    }
                    Instr::Await { dst, value } => {
                        let v = r!(value).clone();
                        let promise = self.promise_resolve_value(v);
                        let frame = self.frames.pop().unwrap();
                        let regs: Vec<Value> = self.stack.drain(frame.base..).collect();
                        let (was_resumed, boundary, ret_reg, p) =
                            (frame.resumed, frame.boundary, frame.ret_reg, frame.async_promise.expect("async frame"));
                        let coro = self.heap.alloc(HeapData::Coroutine(Box::new(Coroutine {
                            state: Some(SuspendedFrame { frame, regs, await_dst: dst }),
                        })));
                        self.promise_then_resume(promise, coro);
                        if was_resumed {
                            return Ok(Value::Undefined);
                        }
                        if boundary {
                            return Ok(Value::Object(p));
                        }
                        if let Some(r) = ret_reg {
                            let cb = self.frames.last().unwrap().base;
                            self.stack[cb + r as usize] = Value::Object(p);
                        }
                        continue 'frames;
                    }
                    Instr::Debugger {} => {
                        if self.debug.active && self.debug_statement(fi, &lf) {
                            self.terminating = true;
                            throw!(Value::Undefined);
                        }
                    }
                }
            }
        }
    }

    /// Elements of an internal argument array (holes read as undefined).
    pub(crate) fn array_elements(&self, a: ObjRef) -> Vec<Value> {
        match &self.heap.obj(a).kind {
            ObjectKind::Array(els) => els.iter().map(|v| if matches!(v, Value::Hole) { Value::Undefined } else { v.clone() }).collect(),
            _ => Vec::new(),
        }
    }
}
