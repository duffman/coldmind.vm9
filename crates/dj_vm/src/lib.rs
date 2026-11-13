//! Bytecode virtual machine for `dj_bytecode` modules.
//!
//! * [`Vm`]: loads validated modules, runs scripts and the event loop.
//! * Values ([`Value`]), objects and a mark-sweep GC ([`heap`]).
//! * Async scheduling: microtask queue, timers, pluggable [`Scheduler`].
//! * Debugger interface: [`DebugHandler`], breakpoints, stepping, pause,
//!   inspection ([`DebugView`]).

pub mod builtins;
pub mod debugger;
pub mod frame;
pub mod heap;
pub mod inspect;
mod interp;
pub mod jobs;
pub mod loader;
pub mod numconv;
pub mod object;
mod ops;
pub mod value;
pub mod vm;

pub use debugger::{
    Breakpoint, BreakpointSpec, DebugHandler, DebugView, ExceptionFilters, ExceptionInfo, FrameInfo, ResumeAction, ScopeInfo,
    StopReason, Variable,
};
pub use heap::{GcStats, ObjRef};
pub use jobs::{Scheduler, SystemClock, VirtualClock};
pub use loader::LoadError;
pub use ops::Hint;
pub use value::{JsStr, PropKey, Symbol, Value};
pub use vm::{ErrorType, Uncaught, Vm, VmOptions};
