//! `dj_ir` — the intermediate representation shared by the frontend and the
//! bytecode generator.
//!
//! The IR is independent of TypeScript (no AST types) and of the VM (no
//! register numbers, no opcodes). See `docs/IR.md` for the specification.
//!
//! Shape:
//! * a [`Module`] is a flat list of [`Function`]s plus an entry function;
//! * a function owns a lexical [`Scope`] tree, [`Local`] variables,
//!   [`Upvalue`] descriptors (captured variables of enclosing functions) and a
//!   control-flow graph of [`Block`]s;
//! * instructions produce single-assignment [`Temp`]s; values that must flow
//!   across control-flow joins go through (synthetic) locals;
//! * every instruction and terminator carries a source [`Span`] and the
//!   [`ScopeId`] it executes in; `Stmt` markers denote statement boundaries
//!   (breakpoint / stepping locations).

pub mod ir;
pub mod passes;
pub mod print;
pub mod source;
pub mod verify;

pub use ir::*;
pub use source::{LineCol, LineIndex, Span};
