//! String, Number and Math.

use super::*;
use crate::numconv::{number_to_radix_string, to_uint16};

pub(crate) fn setup(vm: &mut Vm) {
    // String
    let sp = vm.realm.string_proto;
    let ctor = make_ctor(vm, "String", 1, string_ctor, sp);
    vm.define_method(ctor, "fromCharCode", 1, from_char_code);
    vm.define_method(ctor, "fromCodePoint", 1, from_code_point);
    for (name, len, f) in [
        ("charAt", 1, char_at as NativeFn),
        ("charCodeAt", 1, char_code_at),
        ("codePointAt", 1, code_point_at),
        ("at", 1, str_at),
        ("indexOf", 1, index_of),
        ("lastIndexOf", 1, last_index_of),
        ("includes", 1, includes),
        ("startsWith", 1, starts_with),
        ("endsWith", 1, ends_with),
        ("slice", 2, str_slice),
        ("substring", 2, substring),
        ("substr", 2, substr),
        ("toUpperCase", 0, to_upper),
        ("toLowerCase", 0, to_lower),
        ("toLocaleUpperCase", 0, to_upper),
        ("toLocaleLowerCase", 0, to_lower),
        ("trim", 0, trim),
        ("trimStart", 0, trim_start),
        ("trimEnd", 0, trim_end),
        ("padStart", 2, pad_start),
        ("padEnd", 2, pad_end),
        ("repeat", 1, repeat),
        ("split", 2, split),
        ("replace", 2, replace),
        ("replaceAll", 2, replace_all),
        ("concat", 1, str_concat),
        ("localeCompare", 1, locale_compare),
        ("toString", 0, str_value_of),
        ("valueOf", 0, str_value_of),
    ] {
        vm.define_method(sp, name, len, f);
    }
    let sym = vm.realm.sym_iterator.clone();
    vm.define_symbol_method(sp, sym, "[Symbol.iterator]", 0, str_iterator);
    let sip = vm.realm.string_iter_proto;
    vm.define_method(sip, "next", 0, str_iter_next);
    let tag = vm.realm.sym_to_string_tag.clone();
    vm.define_value(sip, PropKey::Sym(tag), Value::str("String Iterator"), CONFIGURABLE);

    // Number
    let np = vm.realm.number_proto;
    let nctor = make_ctor(vm, "Number", 1, number_ctor, np);
    for (name, v) in [
        ("MAX_SAFE_INTEGER", 9007199254740991.0),
        ("MIN_SAFE_INTEGER", -9007199254740991.0),
        ("EPSILON", f64::EPSILON),
        ("MAX_VALUE", f64::MAX),
        ("MIN_VALUE", 5e-324),
        ("POSITIVE_INFINITY", f64::INFINITY),
        ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
        ("NaN", f64::NAN),
    ] {
        vm.define_value(nctor, name, Value::Number(v), 0);
    }
    vm.define_method(nctor, "isInteger", 1, is_integer);
    vm.define_method(nctor, "isSafeInteger", 1, is_safe_integer);
    vm.define_method(nctor, "isFinite", 1, number_is_finite);
    vm.define_method(nctor, "isNaN", 1, number_is_nan);
    let g = vm.realm.global;
    for name in ["parseFloat", "parseInt"] {
        if let Some(p) = vm.heap.obj(g).props.get(&PropKey::str(name)).cloned() {
            vm.heap.obj_mut(nctor).props.insert(PropKey::str(name), p);
        }
    }
    vm.define_method(np, "toString", 1, number_to_string_method);
    vm.define_method(np, "toLocaleString", 0, number_to_string_method);
    vm.define_method(np, "toFixed", 1, to_fixed);
    vm.define_method(np, "valueOf", 0, number_value_of);

    // Math
    let math = vm.new_object();
    vm.define_value(g, "Math", Value::Object(math), HIDDEN);
    let tag = vm.realm.sym_to_string_tag.clone();
    vm.define_value(math, PropKey::Sym(tag), Value::str("Math"), CONFIGURABLE);
    for (name, v) in [
        ("PI", std::f64::consts::PI),
        ("E", std::f64::consts::E),
        ("LN2", std::f64::consts::LN_2),
        ("LN10", std::f64::consts::LN_10),
        ("LOG2E", std::f64::consts::LOG2_E),
        ("LOG10E", std::f64::consts::LOG10_E),
        ("SQRT2", std::f64::consts::SQRT_2),
        ("SQRT1_2", std::f64::consts::FRAC_1_SQRT_2),
    ] {
        vm.define_value(math, name, Value::Number(v), 0);
    }
    for (name, len, f) in [
        ("abs", 1, math_abs as NativeFn),
        ("floor", 1, math_floor),
        ("ceil", 1, math_ceil),
        ("round", 1, math_round),
        ("trunc", 1, math_trunc),
        ("sign", 1, math_sign),
        ("sqrt", 1, math_sqrt),
        ("cbrt", 1, math_cbrt),
        ("exp", 1, math_exp),
        ("expm1", 1, math_expm1),
        ("log", 1, math_log),
        ("log2", 1, math_log2),
        ("log10", 1, math_log10),
        ("log1p", 1, math_log1p),
        ("sin", 1, math_sin),
        ("cos", 1, math_cos),
        ("tan", 1, math_tan),
        ("asin", 1, math_asin),
        ("acos", 1, math_acos),
        ("atan", 1, math_atan),
        ("sinh", 1, math_sinh),
        ("cosh", 1, math_cosh),
        ("tanh", 1, math_tanh),
        ("fround", 1, math_fround),
        ("clz32", 1, math_clz32),
        ("atan2", 2, math_atan2),
        ("pow", 2, math_pow),
        ("imul", 2, math_imul),
        ("min", 2, math_min),
        ("max", 2, math_max),
        ("hypot", 2, math_hypot),
    ] {
        vm.define_method(math, name, len, f);
    }
    let random = vm.define_method(math, "random", 0, math_random);
    if let ObjectKind::Native(n) = &mut vm.heap.obj_mut(random).kind {
        n.data = vec![Value::Number(f64::from_bits(0x2545_F491_4F6C_DD1D))];
    }
}

// --- String ------------------------------------------------------------------------

fn this_str(vm: &mut Vm, c: &NativeCall, method: &str) -> Result<JsStr, Value> {
    match &c.this {
        Value::String(s) => Ok(s.clone()),
        Value::Object(o) => {
            if let ObjectKind::StringBox(s) = &vm.heap.obj(*o).kind {
                return Ok(s.clone());
            }
            vm.to_string(&c.this)
        }
        Value::Undefined | Value::Null => Err(vm.type_error(format!("String.prototype.{method} called on null or undefined"))),
        v => vm.to_string(v),
    }
}

fn arg_str(vm: &mut Vm, c: &NativeCall, i: usize) -> Result<JsStr, Value> {
    vm.to_string(&c.arg(i))
}

fn string_ctor(vm: &mut Vm, c: &NativeCall) -> R {
    let s = match c.args.first() {
        None => JsStr::default(),
        Some(Value::Symbol(s)) if !c.is_construct() => {
            JsStr::new(&format!("Symbol({})", s.description().map(|d| d.to_rust()).unwrap_or_default()))
        }
        Some(v) => vm.to_string(v)?,
    };
    if c.is_construct() {
        let sp = vm.realm.string_proto;
        let p = proto_from_new_target(vm, &c.new_target, sp)?;
        return Ok(Value::Object(vm.new_object_with(Some(p), ObjectKind::StringBox(s))));
    }
    Ok(Value::String(s))
}

fn from_char_code(vm: &mut Vm, c: &NativeCall) -> R {
    let mut units = Vec::with_capacity(c.args.len());
    for a in &c.args {
        units.push(to_uint16(vm.to_number(a)?));
    }
    Ok(Value::String(JsStr::from_units(units)))
}

fn from_code_point(vm: &mut Vm, c: &NativeCall) -> R {
    let mut units = Vec::new();
    for a in &c.args {
        let n = vm.to_number(a)?;
        if n.fract() != 0.0 || !(0.0..=1114111.0).contains(&n) {
            return Err(vm.range_error(format!("Invalid code point {}", crate::inspect::format_number(n))));
        }
        let cp = n as u32;
        if cp < 0x10000 {
            units.push(cp as u16);
        } else {
            let v = cp - 0x10000;
            units.push(0xD800 + (v >> 10) as u16);
            units.push(0xDC00 + (v & 0x3FF) as u16);
        }
    }
    Ok(Value::String(JsStr::from_units(units)))
}

fn char_at(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_str(vm, c, "charAt")?;
    let i = vm.to_integer(&c.arg(0))?;
    Ok(Value::String(if i >= 0.0 && (i as usize) < s.len() { JsStr::from_units(vec![s.units()[i as usize]]) } else { JsStr::default() }))
}

fn char_code_at(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_str(vm, c, "charCodeAt")?;
    let i = vm.to_integer(&c.arg(0))?;
    Ok(Value::Number(if i >= 0.0 && (i as usize) < s.len() { s.units()[i as usize] as f64 } else { f64::NAN }))
}

fn code_point_at_units(u: &[u16], i: usize) -> (u32, usize) {
    let a = u[i];
    if (0xD800..0xDC00).contains(&a) && i + 1 < u.len() && (0xDC00..0xE000).contains(&u[i + 1]) {
        (0x10000 + (((a as u32) - 0xD800) << 10) + (u[i + 1] as u32 - 0xDC00), 2)
    } else {
        (a as u32, 1)
    }
}

fn code_point_at(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_str(vm, c, "codePointAt")?;
    let i = vm.to_integer(&c.arg(0))?;
    if i < 0.0 || i as usize >= s.len() {
        return Ok(Value::Undefined);
    }
    Ok(Value::Number(code_point_at_units(s.units(), i as usize).0 as f64))
}

fn str_at(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_str(vm, c, "at")?;
    let n = vm.to_integer(&c.arg(0))?;
    let i = if n < 0.0 { s.len() as f64 + n } else { n };
    if i < 0.0 || i as usize >= s.len() {
        return Ok(Value::Undefined);
    }
    Ok(Value::String(JsStr::from_units(vec![s.units()[i as usize]])))
}

fn find_units(hay: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from.min(hay.len()));
    }
    if needle.len() > hay.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

fn index_of(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_str(vm, c, "indexOf")?;
    let n = arg_str(vm, c, 0)?;
    let from = vm.to_integer(&c.arg(1))?.clamp(0.0, s.len() as f64) as usize;
    Ok(Value::Number(find_units(s.units(), n.units(), from).map_or(-1.0, |i| i as f64)))
}

fn last_index_of(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_str(vm, c, "lastIndexOf")?;
    let n = arg_str(vm, c, 0)?;
    let pos = vm.to_number(&c.arg(1))?;
    let from = if pos.is_nan() { f64::INFINITY } else { crate::numconv::to_integer(pos) };
    let (hay, nd) = (s.units(), n.units());
    if nd.len() > hay.len() {
        return Ok(Value::Number(-1.0));
    }
    let start = from.clamp(0.0, (hay.len() - nd.len()) as f64) as usize;
    let r = (0..=start).rev().find(|&i| &hay[i..i + nd.len()] == nd);
    Ok(Value::Number(r.map_or(-1.0, |i| i as f64)))
}

fn includes(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_str(vm, c, "includes")?;
    reject_regexp(vm, &c.arg(0), "includes")?;
    let n = arg_str(vm, c, 0)?;
    let from = vm.to_integer(&c.arg(1))?.clamp(0.0, s.len() as f64) as usize;
    Ok(Value::Bool(find_units(s.units(), n.units(), from).is_some()))
}

fn reject_regexp(vm: &mut Vm, v: &Value, method: &str) -> Result<(), Value> {
    if let Value::Object(o) = v {
        if crate::inspect::constructor_name(vm, *o).as_deref() == Some("RegExp") {
            return Err(vm.type_error(format!("String.prototype.{method}: regular expressions are not supported")));
        }
    }
    Ok(())
}

fn starts_with(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_str(vm, c, "startsWith")?;
    let n = arg_str(vm, c, 0)?;
    let pos = vm.to_integer(&c.arg(1))?.clamp(0.0, s.len() as f64) as usize;
    Ok(Value::Bool(s.units()[pos..].starts_with(n.units())))
}

fn ends_with(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_str(vm, c, "endsWith")?;
    let n = arg_str(vm, c, 0)?;
    let end = if c.arg(1).is_undefined() { s.len() } else { vm.to_integer(&c.arg(1))?.clamp(0.0, s.len() as f64) as usize };
    Ok(Value::Bool(s.units()[..end].ends_with(n.units())))
}

fn sub(s: &JsStr, a: usize, b: usize) -> Value {
    Value::String(if a >= b { JsStr::default() } else { JsStr::from_units(s.units()[a..b].to_vec()) })
}

fn str_slice(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_str(vm, c, "slice")?;
    let a = relative_index(vm, &c.arg(0), s.len(), 0)?;
    let b = relative_index(vm, &c.arg(1), s.len(), s.len())?;
    Ok(sub(&s, a, b))
}

fn substring(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_str(vm, c, "substring")?;
    let len = s.len() as f64;
    let a = vm.to_integer(&c.arg(0))?.clamp(0.0, len) as usize;
    let b = if c.arg(1).is_undefined() { s.len() } else { vm.to_integer(&c.arg(1))?.clamp(0.0, len) as usize };
    Ok(sub(&s, a.min(b), a.max(b)))
}

fn substr(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_str(vm, c, "substr")?;
    let a = relative_index(vm, &c.arg(0), s.len(), 0)?;
    let n = if c.arg(1).is_undefined() { (s.len() - a) as f64 } else { vm.to_integer(&c.arg(1))?.clamp(0.0, (s.len() - a) as f64) };
    Ok(sub(&s, a, a + n as usize))
}

fn to_upper(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_str(vm, c, "toUpperCase")?;
    Ok(Value::str(&s.to_rust().to_uppercase()))
}

fn to_lower(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_str(vm, c, "toLowerCase")?;
    Ok(Value::str(&s.to_rust().to_lowercase()))
}

fn trim_by(vm: &mut Vm, c: &NativeCall, start: bool, end: bool) -> R {
    let s = this_str(vm, c, "trim")?;
    let u = s.units();
    let is_ws = |x: &u16| crate::numconv::is_js_whitespace(*x);
    let a = if start { u.iter().position(|x| !is_ws(x)).unwrap_or(u.len()) } else { 0 };
    let b = if end { u.iter().rposition(|x| !is_ws(x)).map_or(a, |i| i + 1) } else { u.len() };
    Ok(sub(&s, a, b.max(a)))
}

fn trim(vm: &mut Vm, c: &NativeCall) -> R {
    trim_by(vm, c, true, true)
}
fn trim_start(vm: &mut Vm, c: &NativeCall) -> R {
    trim_by(vm, c, true, false)
}
fn trim_end(vm: &mut Vm, c: &NativeCall) -> R {
    trim_by(vm, c, false, true)
}

fn pad(vm: &mut Vm, c: &NativeCall, at_start: bool) -> R {
    let s = this_str(vm, c, "padStart")?;
    let target = vm.to_length(&c.arg(0))? as usize;
    let filler = if c.arg(1).is_undefined() { JsStr::new(" ") } else { arg_str(vm, c, 1)? };
    if target <= s.len() || filler.is_empty() {
        return Ok(Value::String(s));
    }
    if target > (1 << 28) {
        return Err(vm.range_error("Invalid string length"));
    }
    let fill: Vec<u16> = filler.units().iter().copied().cycle().take(target - s.len()).collect();
    let mut out = Vec::with_capacity(target);
    if at_start {
        out.extend(fill);
        out.extend_from_slice(s.units());
    } else {
        out.extend_from_slice(s.units());
        out.extend(fill);
    }
    Ok(Value::String(JsStr::from_units(out)))
}

fn pad_start(vm: &mut Vm, c: &NativeCall) -> R {
    pad(vm, c, true)
}
fn pad_end(vm: &mut Vm, c: &NativeCall) -> R {
    pad(vm, c, false)
}

fn repeat(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_str(vm, c, "repeat")?;
    let n = vm.to_integer(&c.arg(0))?;
    if n < 0.0 || n.is_infinite() {
        return Err(vm.range_error(format!("Invalid count value: {}", crate::inspect::format_number(n))));
    }
    if s.len() as f64 * n > (1u64 << 28) as f64 {
        return Err(vm.range_error("Invalid string length"));
    }
    Ok(Value::String(JsStr::from_units(s.units().repeat(n as usize))))
}

fn split(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_str(vm, c, "split")?;
    reject_regexp(vm, &c.arg(0), "split")?;
    let limit = if c.arg(1).is_undefined() { u32::MAX as usize } else { crate::numconv::to_uint32(vm.to_number(&c.arg(1))?) as usize };
    let sep = c.arg(0);
    let mut out = Vec::new();
    if limit == 0 {
        return Ok(Value::Object(vm.new_array(out)));
    }
    if sep.is_undefined() {
        out.push(Value::String(s));
        return Ok(Value::Object(vm.new_array(out)));
    }
    let sep = vm.to_string(&sep)?;
    let (u, d) = (s.units(), sep.units());
    if d.is_empty() {
        out.extend(u.iter().take(limit).map(|&x| Value::String(JsStr::from_units(vec![x]))));
        return Ok(Value::Object(vm.new_array(out)));
    }
    let mut start = 0;
    while let Some(i) = find_units(u, d, start) {
        out.push(Value::String(JsStr::from_units(u[start..i].to_vec())));
        if out.len() >= limit {
            return Ok(Value::Object(vm.new_array(out)));
        }
        start = i + d.len();
    }
    out.push(Value::String(JsStr::from_units(u[start..].to_vec())));
    Ok(Value::Object(vm.new_array(out)))
}

/// GetSubstitution for string patterns.
fn substitution(matched: &[u16], s: &[u16], pos: usize, repl: &[u16]) -> Vec<u16> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < repl.len() {
        if repl[i] == '$' as u16 && i + 1 < repl.len() {
            match char::from_u32(repl[i + 1] as u32) {
                Some('$') => {
                    out.push('$' as u16);
                    i += 2;
                    continue;
                }
                Some('&') => {
                    out.extend_from_slice(matched);
                    i += 2;
                    continue;
                }
                Some('`') => {
                    out.extend_from_slice(&s[..pos]);
                    i += 2;
                    continue;
                }
                Some('\'') => {
                    out.extend_from_slice(&s[(pos + matched.len()).min(s.len())..]);
                    i += 2;
                    continue;
                }
                _ => {}
            }
        }
        out.push(repl[i]);
        i += 1;
    }
    out
}

fn replace_impl(vm: &mut Vm, c: &NativeCall, all: bool) -> R {
    let s = this_str(vm, c, "replace")?;
    reject_regexp(vm, &c.arg(0), if all { "replaceAll" } else { "replace" })?;
    let pat = arg_str(vm, c, 0)?;
    let repl = c.arg(1);
    let functional = vm.is_callable(&repl);
    let repl_str = if functional { JsStr::default() } else { vm.to_string(&repl)? };
    let (u, p) = (s.units(), pat.units());
    let mut positions = Vec::new();
    let mut from = 0;
    while let Some(i) = find_units(u, p, from) {
        positions.push(i);
        if !all {
            break;
        }
        from = i + p.len().max(1);
        if from > u.len() {
            break;
        }
    }
    let mut out = Vec::new();
    let mut last = 0;
    for pos in positions {
        out.extend_from_slice(&u[last..pos]);
        if functional {
            let r = vm.call(&repl, Value::Undefined, vec![Value::String(pat.clone()), Value::Number(pos as f64), Value::String(s.clone())])?;
            out.extend_from_slice(vm.to_string(&r)?.units());
        } else {
            out.extend(substitution(p, u, pos, repl_str.units()));
        }
        last = pos + p.len();
    }
    out.extend_from_slice(&u[last.min(u.len())..]);
    Ok(Value::String(JsStr::from_units(out)))
}

fn replace(vm: &mut Vm, c: &NativeCall) -> R {
    replace_impl(vm, c, false)
}
fn replace_all(vm: &mut Vm, c: &NativeCall) -> R {
    replace_impl(vm, c, true)
}

fn str_concat(vm: &mut Vm, c: &NativeCall) -> R {
    let mut s = this_str(vm, c, "concat")?;
    for a in &c.args {
        let t = vm.to_string(a)?;
        s = s.concat(&t);
    }
    Ok(Value::String(s))
}

fn locale_compare(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_str(vm, c, "localeCompare")?;
    let t = arg_str(vm, c, 0)?;
    Ok(Value::Number(match s.to_rust().cmp(&t.to_rust()) {
        std::cmp::Ordering::Less => -1.0,
        std::cmp::Ordering::Equal => 0.0,
        std::cmp::Ordering::Greater => 1.0,
    }))
}

fn str_value_of(vm: &mut Vm, c: &NativeCall) -> R {
    match &c.this {
        Value::String(s) => Ok(Value::String(s.clone())),
        Value::Object(o) => match &vm.heap.obj(*o).kind {
            ObjectKind::StringBox(s) => Ok(Value::String(s.clone())),
            _ => Err(vm.type_error("String.prototype.valueOf requires that 'this' be a String")),
        },
        _ => Err(vm.type_error("String.prototype.valueOf requires that 'this' be a String")),
    }
}

fn str_iterator(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_str(vm, c, "[Symbol.iterator]")?;
    let proto = Some(vm.realm.string_iter_proto);
    Ok(Value::Object(vm.new_object_with(proto, ObjectKind::StringIter { s, pos: 0 })))
}

fn str_iter_next(vm: &mut Vm, c: &NativeCall) -> R {
    let Value::Object(it) = c.this else { return Err(vm.type_error("next method called on incompatible receiver")) };
    let next = match &mut vm.heap.obj_mut(it).kind {
        ObjectKind::StringIter { s, pos } => {
            if *pos >= s.len() {
                None
            } else {
                let (_, n) = code_point_at_units(s.units(), *pos);
                let piece = JsStr::from_units(s.units()[*pos..*pos + n].to_vec());
                *pos += n;
                Some(piece)
            }
        }
        _ => return Err(vm.type_error("next method called on incompatible receiver")),
    };
    Ok(match next {
        Some(p) => iter_result(vm, Value::String(p), false),
        None => iter_result(vm, Value::Undefined, true),
    })
}

// --- Number ------------------------------------------------------------------------

fn number_ctor(vm: &mut Vm, c: &NativeCall) -> R {
    let n = if c.args.is_empty() { 0.0 } else { vm.to_number(&c.arg(0))? };
    if c.is_construct() {
        let np = vm.realm.number_proto;
        let p = proto_from_new_target(vm, &c.new_target, np)?;
        return Ok(Value::Object(vm.new_object_with(Some(p), ObjectKind::NumberBox(n))));
    }
    Ok(Value::Number(n))
}

fn this_number(vm: &mut Vm, v: &Value, method: &str) -> Result<f64, Value> {
    match v {
        Value::Number(n) => Ok(*n),
        Value::Object(o) => match vm.heap.obj(*o).kind {
            ObjectKind::NumberBox(n) => Ok(n),
            _ => Err(vm.type_error(format!("Number.prototype.{method} requires that 'this' be a Number"))),
        },
        _ => Err(vm.type_error(format!("Number.prototype.{method} requires that 'this' be a Number"))),
    }
}

fn is_integer(_vm: &mut Vm, c: &NativeCall) -> R {
    Ok(Value::Bool(matches!(c.arg(0), Value::Number(n) if n.is_finite() && n.trunc() == n)))
}

fn is_safe_integer(_vm: &mut Vm, c: &NativeCall) -> R {
    Ok(Value::Bool(matches!(c.arg(0), Value::Number(n) if n.is_finite() && n.trunc() == n && n.abs() <= 9007199254740991.0)))
}

fn number_is_finite(_vm: &mut Vm, c: &NativeCall) -> R {
    Ok(Value::Bool(matches!(c.arg(0), Value::Number(n) if n.is_finite())))
}

fn number_is_nan(_vm: &mut Vm, c: &NativeCall) -> R {
    Ok(Value::Bool(matches!(c.arg(0), Value::Number(n) if n.is_nan())))
}

fn number_to_string_method(vm: &mut Vm, c: &NativeCall) -> R {
    let n = this_number(vm, &c.this, "toString")?;
    let radix = if c.arg(0).is_undefined() { 10.0 } else { vm.to_integer(&c.arg(0))? };
    if !(2.0..=36.0).contains(&radix) {
        return Err(vm.range_error("toString() radix must be between 2 and 36"));
    }
    Ok(Value::str(&if radix == 10.0 { number_to_string(n) } else { number_to_radix_string(n, radix as u32) }))
}

/// Number.prototype.toFixed: exact decimal value, ties rounded up (away from zero in magnitude).
fn to_fixed(vm: &mut Vm, c: &NativeCall) -> R {
    let x = this_number(vm, &c.this, "toFixed")?;
    let f = vm.to_integer(&c.arg(0))?;
    if !(0.0..=100.0).contains(&f) {
        return Err(vm.range_error("toFixed() digits argument must be between 0 and 100"));
    }
    let f = f as usize;
    if !x.is_finite() || x.abs() >= 1e21 {
        return Ok(Value::str(&number_to_string(x)));
    }
    let neg = x < 0.0;
    let a = x.abs();
    // Exact decimal expansion (doubles have at most 1074 fractional digits).
    let exact = format!("{a:.1100}");
    let (int_part, frac) = exact.split_once('.').unwrap();
    let rest = &frac[f..];
    let tie = rest.starts_with('5') && rest[1..].bytes().all(|b| b == b'0');
    let mut s = if tie {
        let mut digits: Vec<u8> = format!("{int_part}{}", &frac[..f]).into_bytes();
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, b'1');
                break;
            }
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
        let d = String::from_utf8(digits).unwrap();
        let split = d.len() - f;
        if f == 0 { d } else { format!("{}.{}", &d[..split], &d[split..]) }
    } else {
        format!("{a:.f$}")
    };
    if neg && s.bytes().any(|b| b.is_ascii_digit() && b != b'0') {
        s.insert(0, '-');
    }
    Ok(Value::str(&s))
}

fn number_value_of(vm: &mut Vm, c: &NativeCall) -> R {
    Ok(Value::Number(this_number(vm, &c.this, "valueOf")?))
}

// --- Math --------------------------------------------------------------------------

macro_rules! math1 {
    ($($name:ident => $f:expr),* $(,)?) => {
        $(fn $name(vm: &mut Vm, c: &NativeCall) -> R {
            let x = arg_number(vm, c, 0)?;
            let f: fn(f64) -> f64 = $f;
            Ok(Value::Number(f(x)))
        })*
    };
}

math1! {
    math_abs => f64::abs,
    math_floor => f64::floor,
    math_ceil => f64::ceil,
    math_round => |x| {
        if !x.is_finite() || x.fract() == 0.0 {
            return x;
        }
        let f = x.floor();
        let r = if x - f >= 0.5 { f + 1.0 } else { f };
        if r == 0.0 && x < 0.0 { -0.0 } else { r }
    },
    math_trunc => f64::trunc,
    math_sign => |x| if x.is_nan() || x == 0.0 { x } else { x.signum() },
    math_sqrt => f64::sqrt,
    math_cbrt => f64::cbrt,
    math_exp => f64::exp,
    math_expm1 => f64::exp_m1,
    math_log => f64::ln,
    math_log2 => f64::log2,
    math_log10 => f64::log10,
    math_log1p => f64::ln_1p,
    math_sin => f64::sin,
    math_cos => f64::cos,
    math_tan => f64::tan,
    math_asin => f64::asin,
    math_acos => f64::acos,
    math_atan => f64::atan,
    math_sinh => f64::sinh,
    math_cosh => f64::cosh,
    math_tanh => f64::tanh,
    math_fround => |x| x as f32 as f64,
    math_clz32 => |x| crate::numconv::to_uint32(x).leading_zeros() as f64,
}

fn math_atan2(vm: &mut Vm, c: &NativeCall) -> R {
    let (y, x) = (arg_number(vm, c, 0)?, arg_number(vm, c, 1)?);
    Ok(Value::Number(y.atan2(x)))
}

fn math_pow(vm: &mut Vm, c: &NativeCall) -> R {
    let (a, b) = (arg_number(vm, c, 0)?, arg_number(vm, c, 1)?);
    Ok(Value::Number(crate::numconv::js_pow(a, b)))
}

fn math_imul(vm: &mut Vm, c: &NativeCall) -> R {
    let (a, b) = (arg_number(vm, c, 0)?, arg_number(vm, c, 1)?);
    Ok(Value::Number(crate::numconv::to_int32(a).wrapping_mul(crate::numconv::to_int32(b)) as f64))
}

fn math_minmax(vm: &mut Vm, c: &NativeCall, max: bool) -> R {
    let mut acc = if max { f64::NEG_INFINITY } else { f64::INFINITY };
    let mut nan = false;
    for a in &c.args {
        let n = vm.to_number(a)?;
        if n.is_nan() {
            nan = true;
            continue;
        }
        let better = if max {
            n > acc || (n == 0.0 && acc == 0.0 && acc.is_sign_negative() && !n.is_sign_negative())
        } else {
            n < acc || (n == 0.0 && acc == 0.0 && n.is_sign_negative())
        };
        if better {
            acc = n;
        }
    }
    Ok(Value::Number(if nan { f64::NAN } else { acc }))
}

fn math_min(vm: &mut Vm, c: &NativeCall) -> R {
    math_minmax(vm, c, false)
}

fn math_max(vm: &mut Vm, c: &NativeCall) -> R {
    math_minmax(vm, c, true)
}

fn math_hypot(vm: &mut Vm, c: &NativeCall) -> R {
    let mut nums = Vec::with_capacity(c.args.len());
    for a in &c.args {
        nums.push(vm.to_number(a)?);
    }
    if nums.iter().any(|n| n.is_infinite()) {
        return Ok(Value::Number(f64::INFINITY));
    }
    Ok(Value::Number(nums.iter().map(|n| n * n).sum::<f64>().sqrt()))
}

/// Deterministic xorshift64* generator (state lives in the function object).
fn math_random(vm: &mut Vm, c: &NativeCall) -> R {
    let ObjectKind::Native(n) = &mut vm.heap.obj_mut(c.callee).kind else { unreachable!() };
    let mut x = match n.data.first() {
        Some(Value::Number(s)) => s.to_bits(),
        _ => 0x2545_F491_4F6C_DD1D,
    };
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    n.data = vec![Value::Number(f64::from_bits(x))];
    let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11;
    Ok(Value::Number(r as f64 / (1u64 << 53) as f64))
}
