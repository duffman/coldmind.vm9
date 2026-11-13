//! Standard library: globals, console, Object, Function, Boolean, Symbol,
//! Error types; the rest lives in the submodules.
//!
//! Rooting rule for natives: arguments, `this` and the callee are rooted
//! by the caller. Any other heap value a native allocates or reads and
//! still needs after calling back into JavaScript (`Vm::call`, getters via
//! `Vm::get`, `to_primitive`, ...) must be rooted with `Vm::push_root`.

pub(crate) mod array;
pub(crate) mod collections;
pub(crate) mod json;
pub(crate) mod promise;
pub(crate) mod string;

pub(crate) use array::array_iter_step;

use crate::heap::ObjRef;
use crate::numconv::{number_to_string, trim_js};
use crate::object::*;
use crate::value::{JsStr, PropKey, Symbol, Value};
use crate::vm::{ErrorType, Vm};

type R = Result<Value, Value>;

// --- helpers -----------------------------------------------------------------------

/// Create a global constructor wired to `proto`.
pub(crate) fn make_ctor(vm: &mut Vm, name: &str, length: u32, f: NativeFn, proto: ObjRef) -> ObjRef {
    let c = vm.new_native_full(name, length, f, Vec::new(), true);
    vm.define_value(c, "prototype", Value::Object(proto), 0);
    vm.define_value(proto, "constructor", Value::Object(c), HIDDEN);
    let g = vm.realm.global;
    vm.define_value(g, name, Value::Object(c), HIDDEN);
    c
}

/// Prototype for an object created by a native constructor (`new.target.prototype`).
pub(crate) fn proto_from_new_target(vm: &mut Vm, nt: &Value, default: ObjRef) -> Result<ObjRef, Value> {
    if nt.is_undefined() {
        return Ok(default);
    }
    Ok(match vm.get(nt, &PropKey::str("prototype"))? {
        Value::Object(p) => p,
        _ => default,
    })
}

pub(crate) fn require_callable(vm: &mut Vm, v: &Value) -> Result<(), Value> {
    if vm.is_callable(v) {
        Ok(())
    } else {
        let s = crate::inspect::inspect_short(vm, v);
        Err(vm.type_error(format!("{s} is not a function")))
    }
}

/// `{ value, done }`.
pub(crate) fn iter_result(vm: &mut Vm, value: Value, done: bool) -> Value {
    let o = vm.new_object();
    let obj = vm.heap.obj_mut(o);
    obj.props.insert(PropKey::str("value"), Property::data(value, DEFAULT_FLAGS));
    obj.props.insert(PropKey::str("done"), Property::data(Value::Bool(done), DEFAULT_FLAGS));
    Value::Object(o)
}

pub(crate) fn arg_number(vm: &mut Vm, c: &NativeCall, i: usize) -> Result<f64, Value> {
    let v = c.arg(i);
    vm.to_number(&v)
}

/// Relative index argument (`slice`, `at`, ...) clamped to `0..=len`.
pub(crate) fn relative_index(vm: &mut Vm, v: &Value, len: usize, default: usize) -> Result<usize, Value> {
    if v.is_undefined() {
        return Ok(default);
    }
    let n = vm.to_integer(v)?;
    let len_f = len as f64;
    Ok(if n < 0.0 { (len_f + n).max(0.0) as usize } else { n.min(len_f) as usize })
}

fn bool_value(vm: &mut Vm, v: &Value) -> Result<bool, Value> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::Object(o) => match vm.heap.obj(*o).kind {
            ObjectKind::BooleanBox(b) => Ok(b),
            _ => Err(vm.type_error("Boolean.prototype.valueOf requires that 'this' be a Boolean")),
        },
        _ => Err(vm.type_error("Boolean.prototype.valueOf requires that 'this' be a Boolean")),
    }
}

// --- setup -------------------------------------------------------------------------

pub(crate) fn setup(vm: &mut Vm) {
    let g = vm.realm.global;
    vm.define_value(g, "globalThis", Value::Object(g), HIDDEN);
    vm.define_value(g, "undefined", Value::Undefined, 0);
    vm.define_value(g, "NaN", Value::Number(f64::NAN), 0);
    vm.define_value(g, "Infinity", Value::Number(f64::INFINITY), 0);
    vm.define_method(g, "parseInt", 2, parse_int);
    vm.define_method(g, "parseFloat", 1, parse_float);
    vm.define_method(g, "isNaN", 1, global_is_nan);
    vm.define_method(g, "isFinite", 1, global_is_finite);
    vm.define_method(g, "eval", 1, unsupported_eval);

    let console = vm.new_object();
    for name in ["log", "info", "debug", "trace"] {
        vm.define_method(console, name, 0, console_log);
    }
    vm.define_method(console, "error", 0, console_error);
    vm.define_method(console, "warn", 0, console_error);
    vm.define_method(console, "dir", 0, console_dir);
    vm.define_value(g, "console", Value::Object(console), HIDDEN);

    setup_object(vm);
    setup_function(vm);
    setup_boolean(vm);
    setup_symbol(vm);
    setup_errors(vm);
    array::setup(vm);
    string::setup(vm);
    json::setup(vm);
    promise::setup(vm);
    collections::setup(vm);

    let ip = vm.realm.iterator_proto;
    let sym = vm.realm.sym_iterator.clone();
    vm.define_symbol_method(ip, sym, "[Symbol.iterator]", 0, return_this);
}

fn return_this(_vm: &mut Vm, c: &NativeCall) -> R {
    Ok(c.this.clone())
}

// --- globals -----------------------------------------------------------------------

fn parse_int(vm: &mut Vm, c: &NativeCall) -> R {
    let s = vm.to_string(&c.arg(0))?;
    let radix = vm.to_number(&c.arg(1))?;
    let mut radix = crate::numconv::to_int32(radix);
    let units = trim_js(s.units());
    let mut i = 0;
    let mut sign = 1.0;
    if let Some(&ch) = units.first() {
        if ch == '-' as u16 || ch == '+' as u16 {
            if ch == '-' as u16 {
                sign = -1.0;
            }
            i = 1;
        }
    }
    let mut strip_prefix = true;
    if radix != 0 {
        if !(2..=36).contains(&radix) {
            return Ok(Value::Number(f64::NAN));
        }
        if radix != 16 {
            strip_prefix = false;
        }
    } else {
        radix = 10;
    }
    if strip_prefix && units.len() >= i + 2 && units[i] == '0' as u16 && (units[i + 1] == 'x' as u16 || units[i + 1] == 'X' as u16) {
        i += 2;
        radix = 16;
    }
    let start = i;
    let mut n = 0f64;
    while i < units.len() {
        let d = match char::from_u32(units[i] as u32).and_then(|ch| ch.to_digit(radix as u32)) {
            Some(d) => d,
            None => break,
        };
        n = n * radix as f64 + d as f64;
        i += 1;
    }
    if i == start {
        return Ok(Value::Number(f64::NAN));
    }
    if radix == 10 && i - start > 15 {
        // Re-parse decimal digits exactly.
        let digits: String = units[start..i].iter().map(|&u| u as u8 as char).collect();
        n = digits.parse::<f64>().unwrap_or(n);
    }
    Ok(Value::Number(sign * n))
}

fn parse_float(vm: &mut Vm, c: &NativeCall) -> R {
    let s = vm.to_string(&c.arg(0))?;
    let units = trim_js(s.units());
    let text: String = units.iter().take_while(|&&u| u < 128).map(|&u| u as u8 as char).collect();
    let b = text.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        i += 1;
    }
    if text[i..].starts_with("Infinity") {
        return Ok(Value::Number(if b.first() == Some(&b'-') { f64::NEG_INFINITY } else { f64::INFINITY }));
    }
    let digits_start = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let mut any = i > digits_start;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let fs = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        any |= i > fs;
    }
    if !any {
        return Ok(Value::Number(f64::NAN));
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        let es = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if j > es {
            i = j;
        }
    }
    let t = text[..i].trim_end_matches('.');
    Ok(Value::Number(t.parse::<f64>().unwrap_or(f64::NAN)))
}

fn global_is_nan(vm: &mut Vm, c: &NativeCall) -> R {
    Ok(Value::Bool(arg_number(vm, c, 0)?.is_nan()))
}

fn global_is_finite(vm: &mut Vm, c: &NativeCall) -> R {
    Ok(Value::Bool(arg_number(vm, c, 0)?.is_finite()))
}

fn unsupported_eval(vm: &mut Vm, _c: &NativeCall) -> R {
    Err(vm.make_error(ErrorType::Error, "eval() is not supported (dynamic code evaluation)"))
}

fn console_log(vm: &mut Vm, c: &NativeCall) -> R {
    let mut s = crate::inspect::format_log(vm, &c.args);
    s.push('\n');
    vm.write_output(&s);
    Ok(Value::Undefined)
}

fn console_error(vm: &mut Vm, c: &NativeCall) -> R {
    let mut s = crate::inspect::format_log(vm, &c.args);
    s.push('\n');
    vm.write_error_output(&s);
    Ok(Value::Undefined)
}

fn console_dir(vm: &mut Vm, c: &NativeCall) -> R {
    let mut s = crate::inspect::inspect(vm, &c.arg(0));
    s.push('\n');
    vm.write_output(&s);
    Ok(Value::Undefined)
}

// --- Object ------------------------------------------------------------------------

fn setup_object(vm: &mut Vm) {
    let op = vm.realm.object_proto;
    let ctor = make_ctor(vm, "Object", 1, object_ctor, op);
    for (name, len, f) in [
        ("keys", 1, object_keys as NativeFn),
        ("values", 1, object_values),
        ("entries", 1, object_entries),
        ("assign", 2, object_assign),
        ("freeze", 1, object_freeze),
        ("isFrozen", 1, object_is_frozen),
        ("preventExtensions", 1, object_prevent_extensions),
        ("isExtensible", 1, object_is_extensible),
        ("create", 2, object_create),
        ("getPrototypeOf", 1, object_get_prototype_of),
        ("setPrototypeOf", 2, object_set_prototype_of),
        ("defineProperty", 3, object_define_property),
        ("defineProperties", 2, object_define_properties),
        ("getOwnPropertyNames", 1, object_get_own_property_names),
        ("getOwnPropertySymbols", 1, object_get_own_property_symbols),
        ("getOwnPropertyDescriptor", 2, object_get_own_property_descriptor),
        ("fromEntries", 1, object_from_entries),
        ("is", 2, object_is),
        ("hasOwn", 2, object_has_own),
    ] {
        vm.define_method(ctor, name, len, f);
    }
    vm.define_method(op, "hasOwnProperty", 1, object_proto_has_own_property);
    vm.define_method(op, "isPrototypeOf", 1, object_proto_is_prototype_of);
    vm.define_method(op, "propertyIsEnumerable", 1, object_proto_property_is_enumerable);
    vm.define_method(op, "toString", 0, object_proto_to_string);
    vm.define_method(op, "toLocaleString", 0, object_proto_to_locale_string);
    vm.define_method(op, "valueOf", 0, object_proto_value_of);
}

fn object_ctor(vm: &mut Vm, c: &NativeCall) -> R {
    let v = c.arg(0);
    if v.is_nullish() {
        let proto = vm.realm.object_proto;
        let p = proto_from_new_target(vm, &c.new_target, proto)?;
        return Ok(Value::Object(vm.new_object_with(Some(p), ObjectKind::Ordinary)));
    }
    Ok(Value::Object(vm.to_object(&v)?))
}

fn object_keys(vm: &mut Vm, c: &NativeCall) -> R {
    let o = vm.to_object(&c.arg(0))?;
    let keys = vm.own_enumerable_string_keys(o).into_iter().map(Value::String).collect();
    Ok(Value::Object(vm.new_array(keys)))
}

fn enumerable_own_values(vm: &mut Vm, c: &NativeCall, entries: bool) -> R {
    let o = vm.to_object(&c.arg(0))?;
    let mark = vm.push_root(Value::Object(o));
    let out = vm.new_array(Vec::new());
    vm.push_root(Value::Object(out));
    let r = (|| {
        for k in vm.own_enumerable_string_keys(o) {
            let key = PropKey::Str(k.clone());
            // Skip keys deleted by an earlier getter.
            if !vm.get_own_property(o, &key).is_some_and(|p| p.enumerable()) {
                continue;
            }
            let v = vm.get_with_receiver(o, &key, &Value::Object(o))?;
            let item = if entries { Value::Object(vm.new_array(vec![Value::String(k), v])) } else { v };
            if let ObjectKind::Array(els) = &mut vm.heap.obj_mut(out).kind {
                els.push(item);
            }
        }
        Ok(Value::Object(out))
    })();
    vm.pop_roots(mark);
    r
}

fn object_values(vm: &mut Vm, c: &NativeCall) -> R {
    enumerable_own_values(vm, c, false)
}

fn object_entries(vm: &mut Vm, c: &NativeCall) -> R {
    enumerable_own_values(vm, c, true)
}

fn object_assign(vm: &mut Vm, c: &NativeCall) -> R {
    let target = vm.to_object(&c.arg(0))?;
    let mark = vm.push_root(Value::Object(target));
    let r = (|| {
        for src in c.args.iter().skip(1) {
            if src.is_nullish() {
                continue;
            }
            let from = vm.to_object(src)?;
            vm.push_root(Value::Object(from));
            for k in vm.own_keys(from) {
                if vm.get_own_property(from, &k).is_some_and(|p| p.enumerable()) {
                    let v = vm.get_with_receiver(from, &k, &Value::Object(from))?;
                    vm.set(&Value::Object(target), k, v)?;
                }
            }
        }
        Ok(Value::Object(target))
    })();
    vm.pop_roots(mark);
    r
}

fn object_freeze(vm: &mut Vm, c: &NativeCall) -> R {
    let v = c.arg(0);
    if let Value::Object(o) = v {
        let obj = vm.heap.obj_mut(o);
        obj.extensible = false;
        obj.elements_frozen = true;
        for p in obj.props.values_mut() {
            p.flags &= !CONFIGURABLE;
            if matches!(p.slot, Slot::Data(_)) {
                p.flags &= !WRITABLE;
            }
        }
    }
    Ok(v)
}

fn object_is_frozen(vm: &mut Vm, c: &NativeCall) -> R {
    let Value::Object(o) = c.arg(0) else { return Ok(Value::Bool(true)) };
    let obj = vm.heap.obj(o);
    let elements_ok = match &obj.kind {
        ObjectKind::Array(els) => obj.elements_frozen || els.iter().all(|v| matches!(v, Value::Hole)),
        _ => true,
    };
    let props_ok = obj.props.values().all(|p| !p.configurable() && (matches!(p.slot, Slot::Accessor { .. }) || !p.writable()));
    Ok(Value::Bool(!obj.extensible && elements_ok && props_ok))
}

fn object_prevent_extensions(vm: &mut Vm, c: &NativeCall) -> R {
    let v = c.arg(0);
    if let Value::Object(o) = v {
        vm.heap.obj_mut(o).extensible = false;
    }
    Ok(v)
}

fn object_is_extensible(vm: &mut Vm, c: &NativeCall) -> R {
    Ok(Value::Bool(matches!(c.arg(0), Value::Object(o) if vm.heap.obj(o).extensible)))
}

fn object_create(vm: &mut Vm, c: &NativeCall) -> R {
    let proto = match c.arg(0) {
        Value::Object(p) => Some(p),
        Value::Null => None,
        v => {
            let s = crate::inspect::inspect_short(vm, &v);
            return Err(vm.type_error(format!("Object prototype may only be an Object or null: {s}")));
        }
    };
    let o = vm.new_object_with(proto, ObjectKind::Ordinary);
    let props = c.arg(1);
    if !props.is_undefined() {
        let mark = vm.push_root(Value::Object(o));
        let r = define_properties(vm, o, &props);
        vm.pop_roots(mark);
        r?;
    }
    Ok(Value::Object(o))
}

fn object_get_prototype_of(vm: &mut Vm, c: &NativeCall) -> R {
    let o = vm.to_object(&c.arg(0))?;
    Ok(vm.heap.obj(o).proto.map_or(Value::Null, Value::Object))
}

fn object_set_prototype_of(vm: &mut Vm, c: &NativeCall) -> R {
    let target = c.arg(0);
    let proto = match c.arg(1) {
        Value::Object(p) => Some(p),
        Value::Null => None,
        _ => return Err(vm.type_error("Object prototype may only be an Object or null")),
    };
    if target.is_nullish() {
        return Err(vm.type_error("Object.setPrototypeOf called on null or undefined"));
    }
    let Value::Object(o) = target else { return Ok(target) };
    if vm.heap.obj(o).proto == proto {
        return Ok(target);
    }
    if !vm.heap.obj(o).extensible {
        return Err(vm.type_error("#<Object> is not extensible"));
    }
    let mut cur = proto;
    while let Some(p) = cur {
        if p == o {
            return Err(vm.type_error("Cyclic __proto__ value"));
        }
        cur = vm.heap.obj(p).proto;
    }
    vm.heap.obj_mut(o).proto = proto;
    Ok(target)
}

/// ToPropertyDescriptor merged over an existing property.
fn to_property(vm: &mut Vm, desc: &Value, existing: Option<Property>) -> Result<Property, Value> {
    let Value::Object(d) = desc else {
        let s = crate::inspect::inspect_short(vm, desc);
        return Err(vm.type_error(format!("Property description must be an object: {s}")));
    };
    let mark = vm.root_mark();
    let field = |vm: &mut Vm, name: &str| -> Result<Option<Value>, Value> {
        let k = PropKey::str(name);
        if vm.has_property(*d, &k)? {
            let v = vm.get_with_receiver(*d, &k, desc)?;
            vm.push_root(v.clone());
            Ok(Some(v))
        } else {
            Ok(None)
        }
    };
    let r = (|| {
        let enumerable = field(vm, "enumerable")?.map(|v| v.truthy());
        let configurable = field(vm, "configurable")?.map(|v| v.truthy());
        let value = field(vm, "value")?;
        let writable = field(vm, "writable")?.map(|v| v.truthy());
        let get = field(vm, "get")?;
        let set = field(vm, "set")?;
        for f in [&get, &set].into_iter().flatten() {
            if !f.is_undefined() && !vm.is_callable(f) {
                let s = crate::inspect::inspect_short(vm, f);
                return Err(vm.type_error(format!("Getter/setter must be a function: {s}")));
            }
        }
        if (get.is_some() || set.is_some()) && (value.is_some() || writable.is_some()) {
            return Err(vm.type_error("Invalid property descriptor. Cannot both specify accessors and a value or writable attribute"));
        }
        let old_flags = existing.as_ref().map_or(0, |p| p.flags);
        let bit = |v: Option<bool>, b: u8| match v {
            Some(true) => b,
            Some(false) => 0,
            None => old_flags & b,
        };
        let mut flags = bit(enumerable, ENUMERABLE) | bit(configurable, CONFIGURABLE);
        let slot = if get.is_some() || set.is_some() {
            let (og, os) = match &existing {
                Some(Property { slot: Slot::Accessor { get, set }, .. }) => (*get, *set),
                _ => (None, None),
            };
            Slot::Accessor {
                get: get.map_or(og, |v| v.as_object()),
                set: set.map_or(os, |v| v.as_object()),
            }
        } else {
            flags |= bit(writable, WRITABLE);
            let old = match &existing {
                Some(Property { slot: Slot::Data(v), .. }) => v.clone(),
                _ => Value::Undefined,
            };
            Slot::Data(value.unwrap_or(old))
        };
        Ok(Property { slot, flags })
    })();
    vm.pop_roots(mark);
    r
}

fn define_property_from(vm: &mut Vm, o: ObjRef, key: PropKey, desc: &Value) -> Result<(), Value> {
    let existing = vm.get_own_property(o, &key);
    let p = to_property(vm, desc, existing)?;
    vm.define_own_property(o, key, p)
}

fn object_define_property(vm: &mut Vm, c: &NativeCall) -> R {
    let Value::Object(o) = c.arg(0) else { return Err(vm.type_error("Object.defineProperty called on non-object")) };
    let key = vm.to_property_key(&c.arg(1))?;
    define_property_from(vm, o, key, &c.arg(2))?;
    Ok(Value::Object(o))
}

fn define_properties(vm: &mut Vm, o: ObjRef, props: &Value) -> Result<(), Value> {
    let p = vm.to_object(props)?;
    let mark = vm.push_root(Value::Object(p));
    let r = (|| {
        for k in vm.own_keys(p) {
            if vm.get_own_property(p, &k).is_some_and(|x| x.enumerable()) {
                let d = vm.get_with_receiver(p, &k, &Value::Object(p))?;
                vm.push_root(d.clone());
                define_property_from(vm, o, k, &d)?;
            }
        }
        Ok(())
    })();
    vm.pop_roots(mark);
    r
}

fn object_define_properties(vm: &mut Vm, c: &NativeCall) -> R {
    let Value::Object(o) = c.arg(0) else { return Err(vm.type_error("Object.defineProperties called on non-object")) };
    define_properties(vm, o, &c.arg(1))?;
    Ok(Value::Object(o))
}

fn object_get_own_property_names(vm: &mut Vm, c: &NativeCall) -> R {
    let o = vm.to_object(&c.arg(0))?;
    let keys = vm.own_keys(o).into_iter().filter(|k| !k.is_symbol()).map(|k| k.to_value()).collect();
    Ok(Value::Object(vm.new_array(keys)))
}

fn object_get_own_property_symbols(vm: &mut Vm, c: &NativeCall) -> R {
    let o = vm.to_object(&c.arg(0))?;
    let keys = vm.own_keys(o).into_iter().filter(|k| k.is_symbol()).map(|k| k.to_value()).collect();
    Ok(Value::Object(vm.new_array(keys)))
}

fn object_get_own_property_descriptor(vm: &mut Vm, c: &NativeCall) -> R {
    let o = vm.to_object(&c.arg(0))?;
    let key = vm.to_property_key(&c.arg(1))?;
    let Some(p) = vm.get_own_property(o, &key) else { return Ok(Value::Undefined) };
    let d = vm.new_object();
    let fv = |o: Option<ObjRef>| o.map_or(Value::Undefined, Value::Object);
    let (writable, enumerable, configurable) = (p.writable(), p.enumerable(), p.configurable());
    let mut fields: Vec<(&str, Value)> = match p.slot {
        Slot::Data(v) => vec![("value", v), ("writable", Value::Bool(writable))],
        Slot::Accessor { get, set } => vec![("get", fv(get)), ("set", fv(set))],
    };
    fields.push(("enumerable", Value::Bool(enumerable)));
    fields.push(("configurable", Value::Bool(configurable)));
    for (k, v) in fields {
        vm.define_value(d, k, v, DEFAULT_FLAGS);
    }
    Ok(Value::Object(d))
}

fn object_from_entries(vm: &mut Vm, c: &NativeCall) -> R {
    let items = vm.iterate_to_vec(&c.arg(0))?;
    let o = vm.new_object();
    let mark = vm.push_root(Value::Object(o));
    for it in &items {
        vm.push_root(it.clone());
    }
    let r = (|| {
        for it in &items {
            if !matches!(it, Value::Object(_)) {
                let s = crate::inspect::inspect_short(vm, it);
                return Err(vm.type_error(format!("Iterator value {s} is not an entry object")));
            }
            let k = vm.get(it, &PropKey::str("0"))?;
            vm.push_root(k.clone());
            let v = vm.get(it, &PropKey::str("1"))?;
            vm.push_root(v.clone());
            let key = vm.to_property_key(&k)?;
            vm.create_data_property(o, key, v)?;
        }
        Ok(Value::Object(o))
    })();
    vm.pop_roots(mark);
    r
}

fn object_is(_vm: &mut Vm, c: &NativeCall) -> R {
    Ok(Value::Bool(c.arg(0).same_value(&c.arg(1))))
}

fn object_has_own(vm: &mut Vm, c: &NativeCall) -> R {
    let o = vm.to_object(&c.arg(0))?;
    let k = vm.to_property_key(&c.arg(1))?;
    Ok(Value::Bool(vm.has_own_property(o, &k)))
}

fn object_proto_has_own_property(vm: &mut Vm, c: &NativeCall) -> R {
    let k = vm.to_property_key(&c.arg(0))?;
    let o = vm.to_object(&c.this)?;
    Ok(Value::Bool(vm.has_own_property(o, &k)))
}

fn object_proto_is_prototype_of(vm: &mut Vm, c: &NativeCall) -> R {
    let Value::Object(v) = c.arg(0) else { return Ok(Value::Bool(false)) };
    let o = vm.to_object(&c.this)?;
    let mut cur = vm.heap.obj(v).proto;
    while let Some(p) = cur {
        if p == o {
            return Ok(Value::Bool(true));
        }
        cur = vm.heap.obj(p).proto;
    }
    Ok(Value::Bool(false))
}

fn object_proto_property_is_enumerable(vm: &mut Vm, c: &NativeCall) -> R {
    let k = vm.to_property_key(&c.arg(0))?;
    let o = vm.to_object(&c.this)?;
    Ok(Value::Bool(vm.get_own_property(o, &k).is_some_and(|p| p.enumerable())))
}

fn object_proto_to_string(vm: &mut Vm, c: &NativeCall) -> R {
    let builtin = match &c.this {
        Value::Undefined => return Ok(Value::str("[object Undefined]")),
        Value::Null => return Ok(Value::str("[object Null]")),
        v => {
            let o = vm.to_object(v)?;
            let tag = match &vm.heap.obj(o).kind {
                ObjectKind::Array(_) => "Array",
                ObjectKind::Closure(_) | ObjectKind::Native(_) => "Function",
                ObjectKind::Error => "Error",
                ObjectKind::BooleanBox(_) => "Boolean",
                ObjectKind::NumberBox(_) => "Number",
                ObjectKind::StringBox(_) => "String",
                _ => "Object",
            };
            let key = PropKey::Sym(vm.realm.sym_to_string_tag.clone());
            match vm.get_with_receiver(o, &key, v)? {
                Value::String(s) => s.to_rust(),
                _ => tag.to_string(),
            }
        }
    };
    Ok(Value::str(&format!("[object {builtin}]")))
}

fn object_proto_to_locale_string(vm: &mut Vm, c: &NativeCall) -> R {
    let f = vm.get(&c.this, &PropKey::str("toString"))?;
    vm.call(&f, c.this.clone(), Vec::new())
}

fn object_proto_value_of(vm: &mut Vm, c: &NativeCall) -> R {
    Ok(Value::Object(vm.to_object(&c.this)?))
}

// --- Function ----------------------------------------------------------------------

fn setup_function(vm: &mut Vm) {
    let fp = vm.realm.function_proto;
    make_ctor(vm, "Function", 1, function_ctor, fp);
    vm.define_method(fp, "call", 1, function_call);
    vm.define_method(fp, "apply", 2, function_apply);
    vm.define_method(fp, "bind", 1, function_bind);
    vm.define_method(fp, "toString", 0, function_to_string);
    vm.define_value(fp, "name", Value::str(""), CONFIGURABLE);
    vm.define_value(fp, "length", Value::Number(0.0), CONFIGURABLE);
}

fn function_ctor(vm: &mut Vm, _c: &NativeCall) -> R {
    Err(vm.make_error(ErrorType::Error, "The Function constructor is not supported (dynamic code evaluation)"))
}

fn function_call(vm: &mut Vm, c: &NativeCall) -> R {
    require_callable(vm, &c.this)?;
    vm.call(&c.this, c.arg(0), c.args.iter().skip(1).cloned().collect())
}

/// CreateListFromArrayLike.
pub(crate) fn list_from_array_like(vm: &mut Vm, v: &Value) -> Result<Vec<Value>, Value> {
    match v {
        Value::Undefined | Value::Null => Ok(Vec::new()),
        Value::Object(o) => {
            if let ObjectKind::Array(els) = &vm.heap.obj(*o).kind {
                return Ok(els.iter().map(|v| if matches!(v, Value::Hole) { Value::Undefined } else { v.clone() }).collect());
            }
            let len = vm.get(v, &PropKey::str("length"))?;
            let len = vm.to_length(&len)? as usize;
            let mark = vm.root_mark();
            let mut out = Vec::with_capacity(len.min(1 << 16));
            for i in 0..len {
                let x = vm.get(v, &PropKey::str(&i.to_string()));
                match x {
                    Ok(x) => {
                        vm.push_root(x.clone());
                        out.push(x);
                    }
                    Err(e) => {
                        vm.pop_roots(mark);
                        return Err(e);
                    }
                }
            }
            vm.pop_roots(mark);
            Ok(out)
        }
        _ => Err(vm.type_error("CreateListFromArrayLike called on non-object")),
    }
}

fn function_apply(vm: &mut Vm, c: &NativeCall) -> R {
    require_callable(vm, &c.this)?;
    let args = list_from_array_like(vm, &c.arg(1))?;
    vm.call(&c.this, c.arg(0), args)
}

fn function_bind(vm: &mut Vm, c: &NativeCall) -> R {
    require_callable(vm, &c.this)?;
    let target = c.this.as_object().unwrap();
    let mut data = vec![c.this.clone(), c.arg(0)];
    data.extend(c.args.iter().skip(1).cloned());
    let bound_args = c.args.len().saturating_sub(1) as f64;
    let len = match vm.get_own_property(target, &PropKey::str("length")) {
        Some(Property { slot: Slot::Data(Value::Number(n)), .. }) => (n - bound_args).max(0.0),
        _ => 0.0,
    };
    let tname = match vm.get_own_property(target, &PropKey::str("name")) {
        Some(Property { slot: Slot::Data(Value::String(s)), .. }) => s.to_rust(),
        _ => String::new(),
    };
    let name = format!("bound {tname}");
    let is_ctor = vm.is_constructor(&c.this);
    let f = vm.new_native_full(&name, 0, bound_function, data, is_ctor);
    vm.define_value(f, "length", Value::Number(len), CONFIGURABLE);
    Ok(Value::Object(f))
}

/// Target of a bound function.
pub(crate) fn bound_target(vm: &Vm, o: ObjRef) -> Option<ObjRef> {
    match &vm.heap.obj(o).kind {
        ObjectKind::Native(n) if std::ptr::fn_addr_eq(n.f, bound_function as NativeFn) => n.data.first().and_then(|v| v.as_object()),
        _ => None,
    }
}

fn bound_function(vm: &mut Vm, c: &NativeCall) -> R {
    let data = match &vm.heap.obj(c.callee).kind {
        ObjectKind::Native(n) => n.data.clone(),
        _ => unreachable!(),
    };
    let target = data[0].clone();
    let mut args: Vec<Value> = data[2..].to_vec();
    args.extend(c.args.iter().cloned());
    if c.is_construct() {
        let nt = if c.new_target.as_object() == Some(c.callee) { target.clone() } else { c.new_target.clone() };
        vm.construct(&target, args, Some(nt))
    } else {
        vm.call(&target, data[1].clone(), args)
    }
}

fn function_to_string(vm: &mut Vm, c: &NativeCall) -> R {
    let Value::Object(f) = &c.this else { return Err(vm.type_error("Function.prototype.toString requires that 'this' be a Function")) };
    if !vm.heap.obj(*f).is_callable() {
        return Err(vm.type_error("Function.prototype.toString requires that 'this' be a Function"));
    }
    let name = vm.function_name(*f);
    let s = match vm.heap.obj(*f).kind {
        ObjectKind::Native(_) => format!("function {name}() {{ [native code] }}"),
        _ => format!("function {name}() {{ [bytecode] }}"),
    };
    Ok(Value::str(&s))
}

// --- Boolean -----------------------------------------------------------------------

fn setup_boolean(vm: &mut Vm) {
    let bp = vm.realm.boolean_proto;
    make_ctor(vm, "Boolean", 1, boolean_ctor, bp);
    vm.define_method(bp, "toString", 0, boolean_to_string);
    vm.define_method(bp, "valueOf", 0, boolean_value_of);
}

fn boolean_ctor(vm: &mut Vm, c: &NativeCall) -> R {
    let b = c.arg(0).truthy();
    if c.is_construct() {
        let bp = vm.realm.boolean_proto;
        let p = proto_from_new_target(vm, &c.new_target, bp)?;
        return Ok(Value::Object(vm.new_object_with(Some(p), ObjectKind::BooleanBox(b))));
    }
    Ok(Value::Bool(b))
}

fn boolean_to_string(vm: &mut Vm, c: &NativeCall) -> R {
    let b = bool_value(vm, &c.this)?;
    Ok(Value::str(if b { "true" } else { "false" }))
}

fn boolean_value_of(vm: &mut Vm, c: &NativeCall) -> R {
    Ok(Value::Bool(bool_value(vm, &c.this)?))
}

// --- Symbol ------------------------------------------------------------------------

fn setup_symbol(vm: &mut Vm) {
    let sp = vm.realm.symbol_proto;
    let ctor = make_ctor(vm, "Symbol", 0, symbol_ctor, sp);
    vm.heap.obj_mut(ctor).kind = ObjectKind::Native(NativeFunction { f: symbol_ctor, data: Vec::new(), constructor: false, name: JsStr::new("Symbol") });
    let (it, tag, hi) = (vm.realm.sym_iterator.clone(), vm.realm.sym_to_string_tag.clone(), vm.realm.sym_has_instance.clone());
    vm.define_value(ctor, "iterator", Value::Symbol(it), 0);
    vm.define_value(ctor, "toStringTag", Value::Symbol(tag.clone()), 0);
    vm.define_value(ctor, "hasInstance", Value::Symbol(hi), 0);
    vm.define_method(sp, "toString", 0, symbol_to_string);
    vm.define_method(sp, "valueOf", 0, symbol_value_of);
    vm.define_getter(sp, "description", symbol_description);
    vm.define_value(sp, PropKey::Sym(tag), Value::str("Symbol"), CONFIGURABLE);
}

fn symbol_ctor(vm: &mut Vm, c: &NativeCall) -> R {
    if c.is_construct() {
        return Err(vm.type_error("Symbol is not a constructor"));
    }
    let d = c.arg(0);
    let desc = if d.is_undefined() { None } else { Some(vm.to_string(&d)?) };
    Ok(Value::Symbol(Symbol::new(desc)))
}

fn this_symbol(vm: &mut Vm, v: &Value) -> Result<Symbol, Value> {
    match v {
        Value::Symbol(s) => Ok(s.clone()),
        Value::Object(o) => match &vm.heap.obj(*o).kind {
            ObjectKind::SymbolBox(s) => Ok(s.clone()),
            _ => Err(vm.type_error("Symbol.prototype method called on incompatible receiver")),
        },
        _ => Err(vm.type_error("Symbol.prototype method called on incompatible receiver")),
    }
}

fn symbol_to_string(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_symbol(vm, &c.this)?;
    Ok(Value::str(&format!("Symbol({})", s.description().map(|d| d.to_rust()).unwrap_or_default())))
}

fn symbol_value_of(vm: &mut Vm, c: &NativeCall) -> R {
    Ok(Value::Symbol(this_symbol(vm, &c.this)?))
}

fn symbol_description(vm: &mut Vm, c: &NativeCall) -> R {
    let s = this_symbol(vm, &c.this)?;
    Ok(s.description().map_or(Value::Undefined, |d| Value::String(d.clone())))
}

// --- Errors ------------------------------------------------------------------------

const ERROR_TYPES: [(ErrorType, &str); 5] = [
    (ErrorType::Error, "Error"),
    (ErrorType::TypeError, "TypeError"),
    (ErrorType::RangeError, "RangeError"),
    (ErrorType::ReferenceError, "ReferenceError"),
    (ErrorType::SyntaxError, "SyntaxError"),
];

fn setup_errors(vm: &mut Vm) {
    let mut base_ctor = None;
    for (i, (t, name)) in ERROR_TYPES.iter().enumerate() {
        let proto = vm.error_proto(*t);
        let ctor = make_ctor(vm, name, 1, error_ctor, proto);
        if let ObjectKind::Native(n) = &mut vm.heap.obj_mut(ctor).kind {
            n.data = vec![Value::Number(i as f64)];
        }
        vm.define_value(proto, "name", Value::str(name), HIDDEN);
        vm.define_value(proto, "message", Value::str(""), HIDDEN);
        match base_ctor {
            None => base_ctor = Some(ctor),
            Some(b) => vm.heap.obj_mut(ctor).proto = Some(b),
        }
    }
    let ep = vm.realm.error_proto;
    vm.define_method(ep, "toString", 0, error_to_string);
}

fn error_ctor(vm: &mut Vm, c: &NativeCall) -> R {
    let idx = match &vm.heap.obj(c.callee).kind {
        ObjectKind::Native(n) => match n.data.first() {
            Some(Value::Number(i)) => *i as usize,
            _ => 0,
        },
        _ => 0,
    };
    let default = vm.error_proto(ERROR_TYPES[idx].0);
    let nt = if c.is_construct() { c.new_target.clone() } else { Value::Object(c.callee) };
    let proto = proto_from_new_target(vm, &nt, default)?;
    let o = vm.new_object_with(Some(proto), ObjectKind::Error);
    let mark = vm.push_root(Value::Object(o));
    let r = (|| {
        let msg = c.arg(0);
        if !msg.is_undefined() {
            let s = vm.to_string(&msg)?;
            vm.define_value(o, "message", Value::String(s), HIDDEN);
        }
        if let Value::Object(opts) = c.arg(1) {
            let k = PropKey::str("cause");
            if vm.has_property(opts, &k)? {
                let cause = vm.get_with_receiver(opts, &k, &c.arg(1))?;
                vm.define_value(o, "cause", cause, HIDDEN);
            }
        }
        let name = vm.get(&Value::Object(o), &PropKey::str("name"))?;
        let name = if name.is_undefined() { "Error".into() } else { vm.to_string(&name)?.to_rust() };
        let message = vm.get(&Value::Object(o), &PropKey::str("message"))?;
        let message = vm.to_string(&message)?.to_rust();
        let header = if message.is_empty() { name } else { format!("{name}: {message}") };
        // The constructor's own native frame is not part of the trace.
        let own = vm.native_stack.pop();
        let stack = vm.capture_stack(&header);
        vm.native_stack.extend(own);
        vm.define_value(o, "stack", Value::str(&stack), HIDDEN);
        Ok(Value::Object(o))
    })();
    vm.pop_roots(mark);
    r
}

fn error_to_string(vm: &mut Vm, c: &NativeCall) -> R {
    if !matches!(c.this, Value::Object(_)) {
        return Err(vm.type_error("Error.prototype.toString called on non-object"));
    }
    let name = vm.get(&c.this, &PropKey::str("name"))?;
    let name = if name.is_undefined() { "Error".to_string() } else { vm.to_string(&name)?.to_rust() };
    let msg = vm.get(&c.this, &PropKey::str("message"))?;
    let msg = if msg.is_undefined() { String::new() } else { vm.to_string(&msg)?.to_rust() };
    Ok(Value::str(&match (name.is_empty(), msg.is_empty()) {
        (_, true) => name,
        (true, false) => msg,
        _ => format!("{name}: {msg}"),
    }))
}
