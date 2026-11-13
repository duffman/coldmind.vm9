//! Activation records.

use crate::heap::{trace_value, ObjRef};
use crate::value::Value;

#[derive(Clone, Debug)]
pub struct Frame {
    pub module: u32,
    pub func: u32,
    /// Function object being executed.
    pub callee: ObjRef,
    /// Index of the next instruction to execute. While an instruction runs,
    /// `pc - 1` is that instruction.
    pub pc: usize,
    /// First register in `Vm::stack`.
    pub base: usize,
    pub nregs: usize,
    /// `Value::Hole` in a derived constructor before `super()`.
    pub this: Value,
    pub new_target: Value,
    /// Exception being handled (read by `GetException`).
    pub exception: Value,
    /// Caller register receiving the result (`None` at a run-loop boundary).
    pub ret_reg: Option<u32>,
    /// Bottom frame of a (possibly nested) run loop: completing it returns
    /// from that loop instead of resuming a caller frame.
    pub boundary: bool,
    /// Invoked via `new`: non-object results are replaced with `this`.
    pub construct: bool,
    /// Async function body: completion settles this promise.
    pub async_promise: Option<ObjRef>,
    /// Resumed from the job queue after an `await` (no caller awaits the
    /// promise as a call result anymore).
    pub resumed: bool,
    /// Unique id (debugger frame identity).
    pub id: u64,
}

impl Frame {
    pub fn trace(&self, out: &mut Vec<ObjRef>) {
        out.push(self.callee);
        trace_value(&self.this, out);
        trace_value(&self.new_target, out);
        trace_value(&self.exception, out);
        out.extend(self.async_promise.iter().copied());
    }
}

/// A suspended async activation (registers moved off the stack).
pub struct Coroutine {
    pub state: Option<SuspendedFrame>,
}

pub struct SuspendedFrame {
    pub frame: Frame,
    pub regs: Vec<Value>,
    pub await_dst: u32,
}

impl Coroutine {
    pub fn trace(&self, out: &mut Vec<ObjRef>) {
        if let Some(s) = &self.state {
            s.frame.trace(out);
            s.regs.iter().for_each(|v| trace_value(v, out));
        }
    }
}

/// A native function activation (for stack traces and the debugger).
#[derive(Clone, Debug)]
pub struct NativeFrame {
    pub name: String,
    /// `frames.len()` when the native was entered.
    pub depth: usize,
}
