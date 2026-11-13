//! `dj_frontend` — TypeScript → `dj_ir`.
//!
//! Pipeline: oxc parser (TypeScript, module goal) → lowering with scope
//! resolution, TDZ analysis and type erasure → verified [`dj_ir::Module`].
//!
//! Constructs the VM cannot execute faithfully are rejected with
//! `code: "unsupported"` diagnostics instead of being approximated.

mod builder;
mod decls;
mod lower_class;
mod lower_expr;
mod lower_func;
mod lower_pattern;
mod lower_stmt;

use std::fmt;

use oxc_allocator::Allocator;
use oxc_parser::{ParseOptions, Parser};
use oxc_span::SourceType;

use dj_ir::LineIndex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub span: dj_ir::Span,
    /// `"parse"`, `"unsupported"` or `"semantic"`.
    pub code: &'static str,
}

/// Parse, check and lower `source` into a verified IR module.
pub fn compile_to_ir(source: &str, file_name: &str) -> Result<dj_ir::Module, Vec<Diagnostic>> {
    let allocator = Allocator::default();
    let source_type = SourceType::ts().with_module(true);
    let options = ParseOptions { preserve_parens: false, ..ParseOptions::default() };
    let ret = Parser::new(&allocator, source, source_type).with_options(options).parse();

    let mut diags: Vec<Diagnostic> = ret
        .diagnostics
        .errors()
        .map(|d| {
            let span = d
                .labels
                .first()
                .map(|l| dj_ir::Span::new(l.offset(), l.offset() + l.len()))
                .unwrap_or_default();
            Diagnostic { severity: Severity::Error, message: d.message.to_string(), span, code: "parse" }
        })
        .collect();
    if !diags.is_empty() || ret.fatal_error {
        if diags.is_empty() {
            diags.push(Diagnostic {
                severity: Severity::Error,
                message: "unrecoverable parse error".into(),
                span: dj_ir::Span::DUMMY,
                code: "parse",
            });
        }
        return Err(diags);
    }

    let mut lowerer = builder::Lowerer::new(source, file_name);
    let module = lowerer.lower_program(&ret.program);
    if lowerer.diags.iter().any(|d| d.severity == Severity::Error) {
        let mut d = lowerer.diags;
        d.sort_by_key(|d| (d.span.start, d.span.end));
        return Err(d);
    }

    #[cfg(debug_assertions)]
    if let Err(errs) = dj_ir::verify::verify_module(&module) {
        panic!(
            "internal error: frontend produced invalid IR:\n{}\n\n{}",
            errs.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("\n"),
            dj_ir::print::print_module(&module)
        );
    }
    Ok(module)
}

/// `file:line:col: error[code]: message` followed by the source line and a
/// caret underline. Columns are 1-based UTF-16 columns.
pub fn render_diagnostics(diags: &[Diagnostic], source: &str, file_name: &str) -> String {
    let idx = LineIndex::new(source.into());
    let mut out = String::new();
    for d in diags {
        let lc = idx.line_col(d.span.start);
        out.push_str(&format!("{file_name}:{}:{}: {}[{}]: {}\n", lc.line, lc.col, d.severity, d.code, d.message));
        let text = idx.line_text(lc.line);
        out.push_str(&format!("  | {text}\n"));
        let end_lc = idx.line_col(d.span.end.max(d.span.start));
        let width = if end_lc.line == lc.line {
            end_lc.col.saturating_sub(lc.col).max(1)
        } else {
            (text.encode_utf16().count() as u32 + 1).saturating_sub(lc.col).max(1)
        };
        out.push_str(&format!("  | {}{}\n", " ".repeat(lc.col as usize - 1), "^".repeat(width as usize)));
    }
    out
}
