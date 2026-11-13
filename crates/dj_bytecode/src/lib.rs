//! `dj_bytecode` — the contract between the compiler and the VM.
//!
//! * [`instr`]: the instruction set (one table drives encoding, decoding,
//!   validation and disassembly);
//! * [`module`]: the module container (constant pool, function table,
//!   exception tables, upvalue descriptors);
//! * [`debug`]: debug metadata (line table, scope tree, variable locations
//!   and live ranges, IR position map, breakpoint resolution);
//! * [`format`]: the binary file format (`.djbc`);
//! * [`validate`]: structural validation required before execution;
//! * [`disasm`]: human-readable listing.
//!
//! See `docs/BYTECODE.md` for the specification.

pub mod debug;
pub mod disasm;
pub mod format;
pub mod instr;
pub mod leb;
pub mod module;
pub mod validate;

pub use debug::*;
pub use instr::{DecodeError, Instr, OperandKind, OPCODE_TABLE};
pub use module::*;
pub use validate::{validate, ValidationError};
