//! Value formatting: `console.log` / `util.inspect` (Node-compatible
//! layout: depth 2, break length 80, compact 3, array grouping) and a short
//! form for error messages and the debugger. Formatting never runs
//! JavaScript: accessors print as `[Getter]` etc.

use std::collections::HashMap;

use crate::heap::ObjRef;
use crate::numconv::number_to_string;
use crate::object::*;
use crate::value::{PropKey, Value};
use crate::vm::Vm;

const DEPTH: usize = 2;
const BREAK_LENGTH: usize = 80;
const COMPACT: usize = 3;
const MAX_ARRAY_LENGTH: usize = 100;

pub fn format_number(n: f64) -> String {
    if n == 0.0 && n.is_sign_negative() { "-0".into() } else { number_to_string(n) }
}

/// `util.inspect(v)`.
pub fn inspect(vm: &Vm, v: &Value) -> String {
    inspect_with(vm, v, false, DEPTH)
}

/// `util.inspect(v, { showHidden, depth })`.
fn inspect_with(vm: &Vm, v: &Value, show_hidden: bool, depth: usize) -> String {
    let mut cx =
        Ctx { vm, seen: Vec::new(), indentation: 0, current_depth: 0, circular: HashMap::new(), show_hidden, depth };
    cx.value(v, 0)
}

/// `console.log(...args)` line (without the newline), with `%s %d %i %f %j %o %O %%`.
pub fn format_log(vm: &Vm, args: &[Value]) -> String {
    let mut parts = Vec::new();
    let mut rest = args;
    if let Some(Value::String(fmt)) = args.first() {
        let mut out = String::new();
        let fmt = fmt.to_rust();
        rest = &args[1..];
        let mut it = fmt.chars().peekable();
        while let Some(c) = it.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match it.peek().copied() {
                Some('%') => {
                    it.next();
                    out.push('%');
                }
                Some(spec @ ('s' | 'd' | 'i' | 'f' | 'j' | 'o' | 'O' | 'c')) if !rest.is_empty() => {
                    it.next();
                    let a = &rest[0];
                    rest = &rest[1..];
                    match spec {
                        's' => match a {
                            Value::String(s) => out.push_str(&s.to_rust()),
                            _ => out.push_str(&inspect(vm, a)),
                        },
                        'd' | 'i' | 'f' => {
                            let n = match a {
                                Value::Number(n) => *n,
                                Value::Bool(b) => *b as u8 as f64,
                                Value::String(s) => crate::numconv::string_to_number(s.units()),
                                Value::Null => 0.0,
                                _ => f64::NAN,
                            };
                            let n = if spec == 'i' { n.trunc() } else { n };
                            out.push_str(&format_number(n));
                        }
                        'c' => {}
                        'o' => out.push_str(&inspect_with(vm, a, true, 4)),
                        _ => out.push_str(&inspect(vm, a)),
                    }
                }
                _ => out.push('%'),
            }
        }
        parts.push(out);
    }
    for a in rest {
        parts.push(match a {
            Value::String(s) => s.to_rust(),
            _ => inspect(vm, a),
        });
    }
    parts.join(" ")
}

/// One-line form for error messages and debugger values.
pub fn inspect_short(vm: &Vm, v: &Value) -> String {
    match v {
        Value::String(s) => {
            let s = s.to_rust();
            if s.chars().count() > 100 {
                format!("{:?}…", s.chars().take(100).collect::<String>())
            } else {
                format!("{s:?}")
            }
        }
        Value::Object(o) => {
            let obj = vm.heap.obj(*o);
            match &obj.kind {
                ObjectKind::Closure(_) | ObjectKind::Native(_) => function_base(vm, *o),
                ObjectKind::Array(els) => format!("Array({})", els.len()),
                ObjectKind::Error => {
                    let name = data_prop(vm, *o, "name").and_then(|v| string_of(&v)).unwrap_or_else(|| "Error".into());
                    match data_prop(vm, *o, "message").and_then(|v| string_of(&v)) {
                        Some(m) if !m.is_empty() => format!("{name}: {m}"),
                        _ => name,
                    }
                }
                ObjectKind::Map(m) => format!("Map({})", m.size),
                ObjectKind::Set(m) => format!("Set({})", m.size),
                _ => format!("#<{}>", constructor_name(vm, *o).unwrap_or_else(|| "Object".into())),
            }
        }
        _ => primitive(v, false),
    }
}

fn string_of(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.to_rust()),
        _ => None,
    }
}

fn primitive(v: &Value, quote: bool) -> String {
    match v {
        Value::Undefined | Value::Hole => "undefined".into(),
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => format_number(*n),
        Value::String(s) => {
            if quote {
                str_escape(&s.to_rust())
            } else {
                s.to_rust()
            }
        }
        Value::Symbol(s) => format!("Symbol({})", s.description().map(|d| d.to_rust()).unwrap_or_default()),
        Value::Object(_) => unreachable!(),
    }
}

/// Node's `strEscape`: single quotes unless the string contains them.
pub fn str_escape(s: &str) -> String {
    let quote = if s.contains('\'') {
        if !s.contains('"') {
            '"'
        } else if !s.contains('`') && !s.contains("${") {
            '`'
        } else {
            '\''
        }
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{b}' => out.push_str("\\x0B"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            '\\' => out.push_str("\\\\"),
            '\'' if quote == '\'' => out.push_str("\\'"),
            c if (c as u32) < 0x20 || (0x7f..=0x9f).contains(&(c as u32)) => out.push_str(&format!("\\x{:02X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Own data property value (no getters, no prototype chain).
fn own_data(vm: &Vm, o: ObjRef, k: &str) -> Option<Value> {
    match vm.get_own_property(o, &PropKey::str(k)) {
        Some(Property { slot: Slot::Data(v), .. }) => Some(v),
        _ => None,
    }
}

/// Data property along the prototype chain (accessors yield `None`).
fn data_prop(vm: &Vm, o: ObjRef, k: &str) -> Option<Value> {
    let key = PropKey::str(k);
    let mut cur = Some(o);
    while let Some(c) = cur {
        match vm.get_own_property(c, &key) {
            Some(Property { slot: Slot::Data(v), .. }) => return Some(v),
            Some(_) => return None,
            None => cur = vm.heap.obj(c).proto,
        }
    }
    None
}

fn function_name(vm: &Vm, f: ObjRef) -> String {
    own_data(vm, f, "name").and_then(|v| string_of(&v)).unwrap_or_default()
}

/// Node's `getConstructorName` (`None` for null-prototype chains).
pub fn constructor_name(vm: &Vm, o: ObjRef) -> Option<String> {
    let mut cur = Some(o);
    while let Some(c) = cur {
        if let Some(Value::Object(f)) = own_data(vm, c, "constructor") {
            if vm.heap.obj(f).is_callable() {
                let name = function_name(vm, f);
                if !name.is_empty() && is_instance(vm, o, f) {
                    return Some(name);
                }
            }
        }
        cur = vm.heap.obj(c).proto;
    }
    None
}

fn is_instance(vm: &Vm, o: ObjRef, ctor: ObjRef) -> bool {
    let Some(Value::Object(proto)) = own_data(vm, ctor, "prototype") else { return false };
    let mut cur = vm.heap.obj(o).proto;
    while let Some(c) = cur {
        if c == proto {
            return true;
        }
        cur = vm.heap.obj(c).proto;
    }
    false
}

fn to_string_tag(vm: &Vm, o: ObjRef) -> String {
    let key = PropKey::Sym(vm.realm.sym_to_string_tag.clone());
    // Own enumerable tags are printed as properties instead.
    if vm.get_own_property(o, &key).is_some_and(|p| p.enumerable()) {
        return String::new();
    }
    let mut cur = Some(o);
    while let Some(c) = cur {
        match vm.get_own_property(c, &key) {
            Some(Property { slot: Slot::Data(Value::String(s)), .. }) => return s.to_rust(),
            Some(_) => return String::new(),
            None => cur = vm.heap.obj(c).proto,
        }
    }
    String::new()
}

fn get_prefix(ctor: &Option<String>, tag: &str, fallback: &str, size: &str) -> String {
    match ctor {
        None if !tag.is_empty() && fallback != tag => format!("[{fallback}{size}: null prototype] [{tag}] "),
        None => format!("[{fallback}{size}: null prototype] "),
        Some(c) if !tag.is_empty() && c != tag => format!("{c}{size} [{tag}] "),
        Some(c) => format!("{c}{size} "),
    }
}

fn is_class_ctor(vm: &Vm, f: ObjRef) -> bool {
    match &vm.heap.obj(f).kind {
        ObjectKind::Closure(c) => {
            let k = vm.modules[c.module as usize].functions[c.func as usize].kind;
            matches!(k, dj_bytecode::FunctionKind::ClassConstructor | dj_bytecode::FunctionKind::DerivedConstructor)
        }
        _ => false,
    }
}

fn is_async_fn(vm: &Vm, f: ObjRef) -> bool {
    match &vm.heap.obj(f).kind {
        ObjectKind::Closure(c) => vm.modules[c.module as usize].functions[c.func as usize].is_async,
        _ => false,
    }
}

fn function_base(vm: &Vm, f: ObjRef) -> String {
    let name = function_name(vm, f);
    let ctor = constructor_name(vm, f);
    if is_class_ctor(vm, f) {
        let mut base = format!("class {}", if name.is_empty() { "(anonymous)" } else { &name });
        match &ctor {
            Some(c) if c != "Function" => base.push_str(&format!(" [{c}]")),
            _ => {}
        }
        match ctor {
            None => base.push_str(" extends [null prototype]"),
            Some(_) => {
                if let Some(p) = vm.heap.obj(f).proto {
                    let sup = function_name(vm, p);
                    if !sup.is_empty() && vm.heap.obj(p).is_callable() {
                        base.push_str(&format!(" extends {sup}"));
                    }
                }
            }
        }
        return format!("[{base}]");
    }
    let ty = if is_async_fn(vm, f) { "AsyncFunction" } else { "Function" };
    let mut base = format!("[{ty}");
    if ctor.is_none() {
        base.push_str(" (null prototype)");
    }
    if name.is_empty() {
        base.push_str(" (anonymous)");
    } else {
        base.push_str(&format!(": {name}"));
    }
    base.push(']');
    if let Some(c) = &ctor {
        if c != ty && c != "Function" {
            base.push_str(&format!(" {c}"));
        }
    }
    base
}

fn is_identifier(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_') && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Extras {
    Object,
    Array,
}

struct Ctx<'a> {
    vm: &'a Vm,
    seen: Vec<ObjRef>,
    indentation: usize,
    current_depth: usize,
    circular: HashMap<ObjRef, usize>,
    /// List non-enumerable keys too, as `[key]: value`.
    show_hidden: bool,
    depth: usize,
}

impl Ctx<'_> {
    fn value(&mut self, v: &Value, recurse: usize) -> String {
        match v {
            Value::Object(o) => self.object(*o, recurse),
            _ => primitive(v, true),
        }
    }

    fn property(&mut self, o: ObjRef, key: &PropKey, recurse: usize, extras: Extras) -> String {
        let hidden = self.vm.get_own_property(o, key).is_some_and(|p| !p.enumerable());
        let str = match self.vm.get_own_property(o, key) {
            Some(Property { slot: Slot::Data(v), .. }) => {
                self.indentation += 2;
                let s = self.value(&v, recurse);
                self.indentation -= 2;
                s
            }
            Some(Property { slot: Slot::Accessor { get, set }, .. }) => match (get.is_some(), set.is_some()) {
                (true, true) => "[Getter/Setter]".into(),
                (true, false) => "[Getter]".into(),
                (false, true) => "[Setter]".into(),
                _ => "undefined".into(),
            },
            None => "undefined".into(),
        };
        if extras == Extras::Array {
            return str;
        }
        let name = match key {
            PropKey::Sym(s) => format!("Symbol({})", s.description().map(|d| d.to_rust()).unwrap_or_default()),
            PropKey::Str(s) => {
                let s = s.to_rust();
                if is_identifier(&s) { s } else { str_escape(&s) }
            }
        };
        if hidden { format!("[{name}]: {str}") } else { format!("{name}: {str}") }
    }

    fn object(&mut self, o: ObjRef, recurse: usize) -> String {
        let vm = self.vm;
        if self.seen.contains(&o) {
            let n = self.circular.len() + 1;
            let id = *self.circular.entry(o).or_insert(n);
            return format!("[Circular *{id}]");
        }
        let obj = vm.heap.obj(o);
        let ctor = constructor_name(vm, o);
        let tag = to_string_tag(vm, o);
        let skip_index = matches!(obj.kind, ObjectKind::Array(_) | ObjectKind::StringBox(_));
        let keys: Vec<PropKey> = vm
            .own_keys(o)
            .into_iter()
            .filter(|k| {
                !(skip_index
                    && (k.as_index().is_some() || (!self.show_hidden && matches!(k, PropKey::Str(s) if s.eq_str("length")))))
            })
            .filter(|k| vm.get_own_property(o, k).is_some_and(|p| self.show_hidden || p.enumerable()))
            .collect();
        let mut base = String::new();
        let mut extras = Extras::Object;
        let braces: (String, String);
        enum Body {
            Array,
            Set,
            Map,
            Promise,
            None,
        }
        let mut body = Body::None;
        match &obj.kind {
            ObjectKind::Array(els) => {
                let prefix = if ctor.as_deref() != Some("Array") || !tag.is_empty() {
                    get_prefix(&ctor, &tag, "Array", &format!("({})", els.len()))
                } else {
                    String::new()
                };
                if els.is_empty() && keys.is_empty() {
                    return format!("{prefix}[]");
                }
                braces = (format!("{prefix}["), "]".into());
                extras = Extras::Array;
                body = Body::Array;
            }
            ObjectKind::Set(m) | ObjectKind::Map(m) => {
                let is_set = matches!(obj.kind, ObjectKind::Set(_));
                let prefix = get_prefix(&ctor, &tag, if is_set { "Set" } else { "Map" }, &format!("({})", m.size));
                if m.size == 0 && keys.is_empty() {
                    return format!("{prefix}{{}}");
                }
                braces = (format!("{prefix}{{"), "}".into());
                body = if is_set { Body::Set } else { Body::Map };
            }
            ObjectKind::Closure(_) | ObjectKind::Native(_) => {
                base = function_base(vm, o);
                if keys.is_empty() {
                    return base;
                }
                braces = ("{".into(), "}".into());
            }
            ObjectKind::Error => {
                base = self.error_base(o);
                if keys.is_empty() {
                    return base;
                }
                braces = ("{".into(), "}".into());
            }
            ObjectKind::BooleanBox(_) | ObjectKind::NumberBox(_) | ObjectKind::StringBox(_) | ObjectKind::SymbolBox(_) => {
                let (ty, inner) = match &obj.kind {
                    ObjectKind::BooleanBox(b) => ("Boolean", b.to_string()),
                    ObjectKind::NumberBox(n) => ("Number", format_number(*n)),
                    ObjectKind::StringBox(s) => ("String", str_escape(&s.to_rust())),
                    ObjectKind::SymbolBox(s) => ("Symbol", primitive(&Value::Symbol(s.clone()), true)),
                    _ => unreachable!(),
                };
                base = format!("[{ty}: {inner}]");
                if keys.is_empty() {
                    return base;
                }
                braces = ("{".into(), "}".into());
            }
            ObjectKind::Promise(_) => {
                braces = (format!("{}{{", get_prefix(&ctor, &tag, "Promise", "")), "}".into());
                body = Body::Promise;
            }
            _ => {
                if ctor.as_deref() == Some("Object") {
                    let open = if tag.is_empty() { "{".to_string() } else { format!("{}{{", get_prefix(&ctor, &tag, "Object", "")) };
                    if keys.is_empty() {
                        return format!("{open}}}");
                    }
                    braces = (open, "}".into());
                } else {
                    let open = format!("{}{{", get_prefix(&ctor, &tag, "Object", ""));
                    if keys.is_empty() {
                        return format!("{open}}}");
                    }
                    braces = (open, "}".into());
                }
            }
        }
        if recurse > self.depth {
            let fallback = match obj.kind {
                ObjectKind::Array(_) => "Array",
                _ => "Object",
            };
            let name = get_prefix(&ctor, &tag, fallback, "");
            let name = name.trim_end();
            return if ctor.is_some() { format!("[{name}]") } else { name.to_string() };
        }
        let recurse = recurse + 1;
        self.seen.push(o);
        self.current_depth = recurse;
        let mut output: Vec<String> = Vec::new();
        let mut numeric = true;
        match body {
            Body::Array => {
                let els: Vec<Value> = match &vm.heap.obj(o).kind {
                    ObjectKind::Array(e) => e.clone(),
                    _ => unreachable!(),
                };
                let len = els.len().min(MAX_ARRAY_LENGTH);
                let mut i = 0;
                while i < len {
                    if matches!(els[i], Value::Hole) {
                        let start = i;
                        while i < els.len() && matches!(els[i], Value::Hole) {
                            i += 1;
                        }
                        let n = i - start;
                        output.push(format!("<{n} empty item{}>", if n > 1 { "s" } else { "" }));
                        numeric = false;
                        continue;
                    }
                    if !matches!(els[i], Value::Number(_)) {
                        numeric = false;
                    }
                    self.indentation += 2;
                    let s = self.value(&els[i], recurse);
                    self.indentation -= 2;
                    output.push(s);
                    i += 1;
                }
                if els.len() > i {
                    let rem = els.len() - i;
                    output.push(format!("... {rem} more item{}", if rem > 1 { "s" } else { "" }));
                }
            }
            Body::Set | Body::Map => {
                let entries: Vec<(Value, Value)> = match &vm.heap.obj(o).kind {
                    ObjectKind::Set(m) | ObjectKind::Map(m) => m.entries.iter().flatten().cloned().collect(),
                    _ => unreachable!(),
                };
                self.indentation += 2;
                for (k, v) in entries {
                    let ks = self.value(&k, recurse);
                    if matches!(body, Body::Map) {
                        let vs = self.value(&v, recurse);
                        output.push(format!("{ks} => {vs}"));
                    } else {
                        output.push(ks);
                    }
                }
                self.indentation -= 2;
            }
            Body::Promise => {
                let state = match &vm.heap.obj(o).kind {
                    ObjectKind::Promise(p) => p.state.clone(),
                    _ => unreachable!(),
                };
                match state {
                    PromiseState::Pending => output.push("<pending>".into()),
                    PromiseState::Fulfilled(v) | PromiseState::Rejected(v) => {
                        self.indentation += 2;
                        let s = self.value(&v, recurse);
                        self.indentation -= 2;
                        let rejected = matches!(vm.heap.obj(o).kind, ObjectKind::Promise(PromiseData { state: PromiseState::Rejected(_), .. }));
                        output.push(if rejected { format!("<rejected> {s}") } else { s });
                    }
                }
            }
            Body::None => {}
        }
        for k in &keys {
            output.push(self.property(o, k, recurse, Extras::Object));
        }
        self.seen.pop();
        if let Some(id) = self.circular.get(&o) {
            let r = format!("<ref *{id}>");
            base = if base.is_empty() { r } else { format!("{r} {base}") };
        }
        self.reduce(output, &base, &braces, extras, recurse, numeric)
    }

    fn error_base(&self, o: ObjRef) -> String {
        let vm = self.vm;
        let stack = own_data(vm, o, "stack").and_then(|v| string_of(&v));
        let name = data_prop(vm, o, "name").and_then(|v| string_of(&v)).unwrap_or_else(|| "Error".into());
        let message = data_prop(vm, o, "message").and_then(|v| string_of(&v)).unwrap_or_default();
        let head = if message.is_empty() { name } else { format!("{name}: {message}") };
        let s = match stack {
            Some(s) if !s.is_empty() => s,
            _ => format!("[{head}]"),
        };
        if self.indentation > 0 {
            s.replace('\n', &format!("\n{}", " ".repeat(self.indentation)))
        } else {
            s
        }
    }

    fn reduce(&mut self, mut output: Vec<String>, base: &str, braces: &(String, String), extras: Extras, recurse: usize, numeric: bool) -> String {
        let entries = output.len();
        if extras == Extras::Array && entries > 6 {
            output = self.group_array_elements(output, numeric);
        }
        if self.current_depth - recurse < COMPACT && entries == output.len() {
            let start = output.len() + self.indentation + braces.0.chars().count() + base.chars().count() + 10;
            if self.below_break_length(&output, start, base) {
                let joined = output.join(", ");
                if !joined.contains('\n') {
                    let b = if base.is_empty() { String::new() } else { format!("{base} ") };
                    return format!("{b}{} {joined} {}", braces.0, braces.1);
                }
            }
        }
        let ind = format!("\n{}", " ".repeat(self.indentation));
        let b = if base.is_empty() { String::new() } else { format!("{base} ") };
        format!("{b}{}{ind}  {}{ind}{}", braces.0, output.join(&format!(",{ind}  ")), braces.1)
    }

    fn below_break_length(&self, output: &[String], start: usize, base: &str) -> bool {
        let mut total = output.len() + start;
        if total + output.len() > BREAK_LENGTH {
            return false;
        }
        for o in output {
            total += o.chars().count();
            if total > BREAK_LENGTH {
                return false;
            }
        }
        base.is_empty() || !base.contains('\n')
    }

    fn group_array_elements(&self, output: Vec<String>, numeric: bool) -> Vec<String> {
        let mut total = 0usize;
        let mut max_len = 0usize;
        let mut out_len = output.len();
        if output.last().is_some_and(|s| s.starts_with("... ") && s.contains(" more item")) {
            out_len -= 1;
        }
        let sep = 2;
        let data_len: Vec<usize> = output[..out_len].iter().map(|s| s.chars().count()).collect();
        for &l in &data_len {
            total += l + sep;
            max_len = max_len.max(l);
        }
        let actual_max = max_len + sep;
        if actual_max * 3 + self.indentation < BREAK_LENGTH && (total as f64 / actual_max as f64 > 5.0 || max_len <= 6) {
            let average_bias = (actual_max as f64 - total as f64 / output.len() as f64).sqrt();
            let biased_max = (actual_max as f64 - 3.0 - average_bias).max(1.0);
            let columns = ((2.5 * biased_max * out_len as f64).sqrt() / biased_max)
                .round()
                .min(((BREAK_LENGTH - self.indentation) / actual_max) as f64)
                .min((COMPACT * 4) as f64)
                .min(15.0) as usize;
            if columns <= 1 {
                return output;
            }
            let mut max_line = Vec::new();
            for i in 0..columns {
                let mut line = 0;
                let mut j = i;
                while j < out_len {
                    line = line.max(data_len[j]);
                    j += columns;
                }
                max_line.push(line + sep);
            }
            let mut tmp = Vec::new();
            let mut i = 0;
            while i < out_len {
                let max = (i + columns).min(out_len);
                let mut s = String::new();
                let mut j = i;
                while j < max - 1 {
                    let cell = format!("{}, ", output[j]);
                    let w = max_line[j - i];
                    if numeric {
                        s.push_str(&format!("{cell:>w$}"));
                    } else {
                        s.push_str(&format!("{cell:<w$}"));
                    }
                    j += 1;
                }
                if numeric {
                    let w = max_line[j - i] - sep;
                    s.push_str(&format!("{:>w$}", output[j]));
                } else {
                    s.push_str(&output[j]);
                }
                tmp.push(s);
                i += columns;
            }
            if out_len < output.len() {
                tmp.push(output[out_len].clone());
            }
            return tmp;
        }
        output
    }
}
