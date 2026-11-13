//! Human-readable IR dump (`djungle ir file.ts`).

use std::fmt::Write;

use crate::ir::*;
use crate::source::LineIndex;

pub fn print_module(m: &Module) -> String {
    let idx = LineIndex::new(m.source.clone());
    let mut out = String::new();
    let _ = writeln!(out, "module {:?} entry={}", m.name, m.entry);
    for f in &m.functions {
        out.push('\n');
        print_function_into(&mut out, f, Some(&idx));
    }
    out
}

pub fn print_function(f: &Function) -> String {
    let mut out = String::new();
    print_function_into(&mut out, f, None);
    out
}

fn loc(idx: Option<&LineIndex>, span: crate::Span) -> String {
    match idx {
        Some(i) => i.line_col(span.start).to_string(),
        None => format!("{}..{}", span.start, span.end),
    }
}

fn print_function_into(out: &mut String, f: &Function, idx: Option<&LineIndex>) {
    let _ = write!(out, "function {} {:?} ({}", f.id, &*f.name, f.kind.as_str());
    if f.is_async {
        out.push_str(", async");
    }
    let _ = write!(out, ", params={}", f.param_count);
    if f.has_rest {
        out.push_str(", rest");
    }
    out.push(')');
    if let Some(p) = f.parent {
        let _ = write!(out, " parent={p}");
    }
    let _ = writeln!(out, " @{}", loc(idx, f.span));

    for (i, s) in f.scopes.iter().enumerate() {
        let _ = write!(out, "  scope s{i} {}", s.kind.as_str());
        if let Some(p) = s.parent {
            let _ = write!(out, " parent={p}");
        }
        let _ = writeln!(out, " @{}", loc(idx, s.span));
    }
    for (i, l) in f.locals.iter().enumerate() {
        let _ = write!(out, "  local l{i} {:?} {} in {}", &*l.name, l.kind.as_str(), l.scope);
        if l.captured {
            out.push_str(" captured");
        }
        if l.needs_tdz {
            out.push_str(" tdz");
        }
        out.push('\n');
    }
    for (i, u) in f.upvalues.iter().enumerate() {
        let src = match u.source {
            CaptureSource::ParentLocal(l) => format!("parent {l}"),
            CaptureSource::ParentUpval(v) => format!("parent {v}"),
        };
        let _ = writeln!(out, "  upval u{i} {:?} {} <- {src}", &*u.name, u.kind.as_str());
    }
    for (i, b) in f.blocks.iter().enumerate() {
        let _ = write!(out, "  bb{i}:");
        if BlockId(i as u32) == f.entry {
            out.push_str(" (entry)");
        }
        if let Some(h) = b.handler {
            let _ = write!(out, " handler={h}");
        }
        out.push('\n');
        for inst in &b.insts {
            let text = inst_to_string(&inst.kind);
            let _ = writeln!(out, "    {text:<48} ; {} {}", loc(idx, inst.span), inst.scope);
        }
        let text = term_to_string(&b.term.kind);
        let _ = writeln!(out, "    {text:<48} ; {} {}", loc(idx, b.term.span), b.term.scope);
    }
}

fn list(ts: &[Temp]) -> String {
    ts.iter().map(|t| t.to_string()).collect::<Vec<_>>().join(", ")
}

fn opt(t: &Option<Temp>) -> String {
    t.map(|t| t.to_string()).unwrap_or_else(|| "undefined".into())
}

pub fn constant_to_string(c: &Constant) -> String {
    match c {
        Constant::Undefined => "undefined".into(),
        Constant::Null => "null".into(),
        Constant::Bool(b) => b.to_string(),
        Constant::Number(n) => {
            if *n == 0.0 && n.is_sign_negative() {
                "-0".into()
            } else {
                format!("{n}")
            }
        }
        Constant::String(s) => format!("{:?}", &**s),
    }
}

pub fn inst_to_string(k: &InstKind) -> String {
    use InstKind::*;
    match k {
        Stmt => "stmt".into(),
        Debugger => "debugger".into(),
        Const { dst, value } => format!("{dst} = const {}", constant_to_string(value)),
        LoadLocal { dst, local, check_tdz } => {
            format!("{dst} = load {local}{}", if *check_tdz { " checked" } else { "" })
        }
        StoreLocal { local, src } => format!("store {local}, {src}"),
        DeclareLocal { local } => format!("declare {local}"),
        CopyBinding { local } => format!("copybinding {local}"),
        LoadUpval { dst, upval, check_tdz } => {
            format!("{dst} = load {upval}{}", if *check_tdz { " checked" } else { "" })
        }
        StoreUpval { upval, src } => format!("store {upval}, {src}"),
        LoadGlobal { dst, name } => format!("{dst} = global {:?}", &**name),
        StoreGlobal { name, src } => format!("setglobal {:?}, {src}", &**name),
        TypeofGlobal { dst, name } => format!("{dst} = typeofglobal {:?}", &**name),
        LoadThis { dst } => format!("{dst} = this"),
        LoadNewTarget { dst } => format!("{dst} = new.target"),
        LoadCallee { dst } => format!("{dst} = callee"),
        Unary { dst, op, src } => format!("{dst} = {} {src}", op.as_str()),
        Binary { dst, op, lhs, rhs } => format!("{dst} = {} {lhs}, {rhs}", op.as_str()),
        GetProp { dst, obj, key } => format!("{dst} = {obj}[{key}]"),
        GetNamed { dst, obj, name } => format!("{dst} = {obj}.{name}"),
        SetProp { obj, key, value } => format!("{obj}[{key}] = {value}"),
        SetNamed { obj, name, value } => format!("{obj}.{name} = {value}"),
        DeleteProp { dst, obj, key } => format!("{dst} = delete {obj}[{key}]"),
        NewObject { dst } => format!("{dst} = {{}}"),
        NewArray { dst } => format!("{dst} = []"),
        ArrayPush { array, value } => format!("push {array}, {value}"),
        ArraySpread { array, iterable } => format!("spread {array}, {iterable}"),
        ArrayHole { array } => format!("hole {array}"),
        DefineField { obj, key, value } => format!("define {obj}[{key}] = {value}"),
        DefineNamed { obj, name, value } => format!("define {obj}.{name} = {value}"),
        DefineMethod { obj, key, func, kind, enumerable } => format!(
            "define{} {obj}[{key}] = {func}{}",
            match kind {
                MethodKind::Method => "method",
                MethodKind::Getter => "getter",
                MethodKind::Setter => "setter",
            },
            if *enumerable { " enumerable" } else { "" }
        ),
        CopyDataProps { target, source, excluded } => {
            format!("copyprops {target}, {source} except [{}]", list(excluded))
        }
        MakeClosure { dst, func } => format!("{dst} = closure {func}"),
        MakeClass { dst, ctor, parent } => format!("{dst} = class {ctor} extends {}", opt(parent)),
        SetFieldInit { class, init } => format!("setfieldinit {class}, {init}"),
        InitFields => "initfields".into(),
        Call { dst, callee, this, args } => format!("{dst} = call {callee} this={} ({})", opt(this), list(args)),
        CallSpread { dst, callee, this, args } => format!("{dst} = call {callee} this={} (...{args})", opt(this)),
        New { dst, callee, args } => format!("{dst} = new {callee} ({})", list(args)),
        NewSpread { dst, callee, args } => format!("{dst} = new {callee} (...{args})"),
        SuperCall { dst, args } => format!("{dst} = super ({})", list(args)),
        SuperCallSpread { dst, args } => format!("{dst} = super (...{args})"),
        SuperGet { dst, key } => format!("{dst} = super[{key}]"),
        SuperSet { key, value } => format!("super[{key}] = {value}"),
        GetIterator { dst, iterable } => format!("{dst} = getiter {iterable}"),
        IterNext { done, value, iter } => format!("{done}, {value} = iternext {iter}"),
        IterClose { iter } => format!("iterclose {iter}"),
        ForInIterator { dst, obj } => format!("{dst} = forin {obj}"),
        Await { dst, value } => format!("{dst} = await {value}"),
        CatchValue { dst } => format!("{dst} = catch"),
    }
}

pub fn term_to_string(k: &TermKind) -> String {
    match k {
        TermKind::Jump(b) => format!("jump {b}"),
        TermKind::Branch { cond, then_bb, else_bb } => format!("branch {cond}, {then_bb}, {else_bb}"),
        TermKind::Return(t) => format!("return {t}"),
        TermKind::Throw(t) => format!("throw {t}"),
        TermKind::ThrowError { kind, message } => format!("throwerror {} {:?}", kind.as_str(), &**message),
        TermKind::Unreachable => "unreachable".into(),
    }
}
