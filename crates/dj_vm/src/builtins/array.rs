//! Array constructor, prototype and array iterators.
//!
//! Read-only methods are generic over array-likes; mutating methods require
//! a real Array (`this` of another kind throws a TypeError).

use super::*;

pub(crate) fn setup(vm: &mut Vm) {
    let ap = vm.realm.array_proto;
    let ctor = make_ctor(vm, "Array", 1, array_ctor, ap);
    vm.define_method(ctor, "isArray", 1, array_is_array);
    vm.define_method(ctor, "from", 1, array_from);
    vm.define_method(ctor, "of", 0, array_of);
    for (name, len, f) in [
        ("push", 1, push as NativeFn),
        ("pop", 0, pop),
        ("shift", 0, shift),
        ("unshift", 1, unshift),
        ("slice", 2, slice),
        ("splice", 2, splice),
        ("concat", 1, concat),
        ("join", 1, join),
        ("toString", 0, to_string),
        ("reverse", 0, reverse),
        ("indexOf", 1, index_of),
        ("lastIndexOf", 1, last_index_of),
        ("includes", 1, includes),
        ("find", 1, find),
        ("findIndex", 1, find_index),
        ("findLast", 1, find_last),
        ("findLastIndex", 1, find_last_index),
        ("filter", 1, filter),
        ("map", 1, map),
        ("forEach", 1, for_each),
        ("some", 1, some),
        ("every", 1, every),
        ("reduce", 1, reduce),
        ("reduceRight", 1, reduce_right),
        ("sort", 1, sort),
        ("fill", 1, fill),
        ("keys", 0, keys),
        ("entries", 0, entries),
        ("flat", 0, flat),
        ("flatMap", 1, flat_map),
        ("at", 1, at),
        ("copyWithin", 2, copy_within),
        ("toSorted", 1, to_sorted),
        ("toReversed", 0, to_reversed),
        ("with", 2, with),
    ] {
        vm.define_method(ap, name, len, f);
    }
    let values_fn = vm.define_method(ap, "values", 0, values);
    let sym = vm.realm.sym_iterator.clone();
    vm.define_value(ap, PropKey::Sym(sym), Value::Object(values_fn), HIDDEN);

    let aip = vm.realm.array_iter_proto;
    let next = vm.define_method(aip, "next", 0, array_iter_next);
    vm.realm.array_iter_next = Some(next);
    let tag = vm.realm.sym_to_string_tag.clone();
    vm.define_value(aip, PropKey::Sym(tag), Value::str("Array Iterator"), CONFIGURABLE);
}

fn index_key(i: usize) -> PropKey {
    PropKey::str(&i.to_string())
}

pub(crate) fn len_of(vm: &mut Vm, o: ObjRef) -> Result<usize, Value> {
    if let ObjectKind::Array(els) = &vm.heap.obj(o).kind {
        return Ok(els.len());
    }
    let l = vm.get(&Value::Object(o), &PropKey::str("length"))?;
    Ok(vm.to_length(&l)? as usize)
}

/// Element `i`, `None` for holes / absent indices.
pub(crate) fn get_at(vm: &mut Vm, o: ObjRef, i: usize) -> Result<Option<Value>, Value> {
    if let ObjectKind::Array(els) = &vm.heap.obj(o).kind {
        return Ok(match els.get(i) {
            Some(Value::Hole) | None => None,
            Some(v) => Some(v.clone()),
        });
    }
    let k = index_key(i);
    if vm.has_property(o, &k)? { Ok(Some(vm.get_with_receiver(o, &k, &Value::Object(o))?)) } else { Ok(None) }
}

fn els_mut(vm: &mut Vm, o: ObjRef) -> &mut Vec<Value> {
    match &mut vm.heap.obj_mut(o).kind {
        ObjectKind::Array(els) => els,
        _ => unreachable!(),
    }
}

/// `this` as a mutable Array: `grow` also requires extensibility.
fn this_array(vm: &mut Vm, c: &NativeCall, method: &str, grow: bool) -> Result<ObjRef, Value> {
    let Value::Object(o) = c.this else {
        return Err(vm.type_error(format!("Array.prototype.{method} called on non-array")));
    };
    let obj = vm.heap.obj(o);
    if !obj.is_array() {
        return Err(vm.type_error(format!("Array.prototype.{method} is only supported on arrays")));
    }
    if obj.elements_frozen || (grow && !obj.extensible) {
        return Err(vm.type_error(format!("Cannot modify a frozen or non-extensible array (Array.prototype.{method})")));
    }
    Ok(o)
}

fn new_array_from(vm: &mut Vm, v: Vec<Value>) -> Value {
    Value::Object(vm.new_array(v))
}

/// Run `f` with `o` rooted.
fn rooted<T>(vm: &mut Vm, o: &[Value], f: impl FnOnce(&mut Vm) -> Result<T, Value>) -> Result<T, Value> {
    let mark = vm.root_mark();
    for v in o {
        vm.push_root(v.clone());
    }
    let r = f(vm);
    vm.pop_roots(mark);
    r
}

fn array_ctor(vm: &mut Vm, c: &NativeCall) -> R {
    let ap = vm.realm.array_proto;
    let nt = if c.is_construct() { c.new_target.clone() } else { Value::Undefined };
    let proto = proto_from_new_target(vm, &nt, ap)?;
    let els = match c.args.as_slice() {
        [Value::Number(n)] => {
            let n = *n;
            if n < 0.0 || n.fract() != 0.0 || n > u32::MAX as f64 {
                return Err(vm.range_error("Invalid array length"));
            }
            if n > crate::ops::MAX_DENSE_LENGTH as f64 {
                return Err(vm.range_error("Invalid array length (exceeds the maximum dense array length)"));
            }
            vec![Value::Hole; n as usize]
        }
        args => args.to_vec(),
    };
    Ok(Value::Object(vm.new_object_with(Some(proto), ObjectKind::Array(els))))
}

fn array_is_array(vm: &mut Vm, c: &NativeCall) -> R {
    Ok(Value::Bool(vm.is_array(&c.arg(0))))
}

fn array_from(vm: &mut Vm, c: &NativeCall) -> R {
    let items = c.arg(0);
    let map_fn = c.arg(1);
    if !map_fn.is_undefined() {
        require_callable(vm, &map_fn)?;
    }
    if items.is_nullish() {
        let s = crate::inspect::inspect_short(vm, &items);
        return Err(vm.type_error(format!("{s} is not iterable")));
    }
    let key = PropKey::Sym(vm.realm.sym_iterator.clone());
    let iter_fn = vm.get(&items, &key)?;
    let list = if iter_fn.is_nullish() { list_from_array_like(vm, &items)? } else { vm.iterate_to_vec(&items)? };
    let out = vm.new_array(list);
    if map_fn.is_undefined() {
        return Ok(Value::Object(out));
    }
    rooted(vm, &[Value::Object(out)], |vm| {
        let n = len_of(vm, out)?;
        for i in 0..n {
            let v = els_mut(vm, out)[i].clone();
            let r = vm.call(&map_fn, c.arg(2), vec![v, Value::Number(i as f64)])?;
            els_mut(vm, out)[i] = r;
        }
        Ok(Value::Object(out))
    })
}

fn array_of(vm: &mut Vm, c: &NativeCall) -> R {
    Ok(new_array_from(vm, c.args.clone()))
}

fn push(vm: &mut Vm, c: &NativeCall) -> R {
    let o = this_array(vm, c, "push", true)?;
    let els = els_mut(vm, o);
    if els.len() + c.args.len() > crate::ops::MAX_DENSE_LENGTH as usize {
        return Err(vm.range_error("Invalid array length (exceeds the maximum dense array length)"));
    }
    els.extend(c.args.iter().cloned());
    Ok(Value::Number(els.len() as f64))
}

fn pop(vm: &mut Vm, c: &NativeCall) -> R {
    let o = this_array(vm, c, "pop", false)?;
    Ok(match els_mut(vm, o).pop() {
        Some(Value::Hole) | None => Value::Undefined,
        Some(v) => v,
    })
}

fn shift(vm: &mut Vm, c: &NativeCall) -> R {
    let o = this_array(vm, c, "shift", false)?;
    let els = els_mut(vm, o);
    if els.is_empty() {
        return Ok(Value::Undefined);
    }
    Ok(match els.remove(0) {
        Value::Hole => Value::Undefined,
        v => v,
    })
}

fn unshift(vm: &mut Vm, c: &NativeCall) -> R {
    let o = this_array(vm, c, "unshift", true)?;
    let els = els_mut(vm, o);
    els.splice(0..0, c.args.iter().cloned());
    Ok(Value::Number(els.len() as f64))
}

fn slice(vm: &mut Vm, c: &NativeCall) -> R {
    let o = vm.to_object(&c.this)?;
    rooted(vm, &[Value::Object(o)], |vm| {
        let len = len_of(vm, o)?;
        let start = relative_index(vm, &c.arg(0), len, 0)?;
        let end = relative_index(vm, &c.arg(1), len, len)?;
        let mut out = Vec::with_capacity(end.saturating_sub(start));
        let mark = vm.root_mark();
        for i in start..end {
            let v = get_at(vm, o, i)?.unwrap_or(Value::Hole);
            vm.push_root(v.clone());
            out.push(v);
        }
        vm.pop_roots(mark);
        Ok(new_array_from(vm, out))
    })
}

fn splice(vm: &mut Vm, c: &NativeCall) -> R {
    let o = this_array(vm, c, "splice", c.args.len() > 2)?;
    let len = els_mut(vm, o).len();
    let start = relative_index(vm, &c.arg(0), len, 0)?;
    let del = match c.args.len() {
        0 => 0,
        1 => len - start,
        _ => {
            let n = vm.to_integer(&c.arg(1))?;
            (n.max(0.0) as usize).min(len - start)
        }
    };
    // Conversions may have run user code that shrank the array.
    let els = els_mut(vm, o);
    let start = start.min(els.len());
    let end = (start + del).min(els.len());
    let removed: Vec<Value> = els.splice(start..end, c.args.iter().skip(2).cloned()).collect();
    Ok(new_array_from(vm, removed))
}

fn concat(vm: &mut Vm, c: &NativeCall) -> R {
    let this = vm.to_object(&c.this)?;
    let mut out = Vec::new();
    for v in std::iter::once(Value::Object(this)).chain(c.args.iter().cloned()) {
        match &v {
            Value::Object(o) if vm.heap.obj(*o).is_array() => {
                if let ObjectKind::Array(els) = &vm.heap.obj(*o).kind {
                    out.extend(els.iter().cloned());
                }
            }
            _ => out.push(v),
        }
    }
    Ok(new_array_from(vm, out))
}

fn join_impl(vm: &mut Vm, o: ObjRef, sep: &Value) -> R {
    rooted(vm, &[Value::Object(o)], |vm| {
        let len = len_of(vm, o)?;
        let sep = if sep.is_undefined() { JsStr::new(",") } else { vm.to_string(sep)? };
        let mut out: Vec<u16> = Vec::new();
        for i in 0..len {
            if i > 0 {
                out.extend_from_slice(sep.units());
            }
            match get_at(vm, o, i)? {
                None | Some(Value::Undefined) | Some(Value::Null) => {}
                Some(v) => out.extend_from_slice(vm.to_string(&v)?.units()),
            }
        }
        Ok(Value::String(JsStr::from_units(out)))
    })
}

fn join(vm: &mut Vm, c: &NativeCall) -> R {
    let o = vm.to_object(&c.this)?;
    join_impl(vm, o, &c.arg(0))
}

fn to_string(vm: &mut Vm, c: &NativeCall) -> R {
    let o = vm.to_object(&c.this)?;
    join_impl(vm, o, &Value::Undefined)
}

fn reverse(vm: &mut Vm, c: &NativeCall) -> R {
    let o = this_array(vm, c, "reverse", false)?;
    els_mut(vm, o).reverse();
    Ok(Value::Object(o))
}

fn search(vm: &mut Vm, c: &NativeCall, backwards: bool, zero: bool) -> Result<Option<usize>, Value> {
    let o = vm.to_object(&c.this)?;
    rooted(vm, &[Value::Object(o)], |vm| {
        let len = len_of(vm, o)?;
        if len == 0 {
            return Ok(None);
        }
        let target = c.arg(0);
        let indices: Box<dyn Iterator<Item = usize>> = if backwards {
            let from = if c.args.len() > 1 {
                let n = vm.to_integer(&c.arg(1))?;
                if n < 0.0 { len as f64 + n } else { n.min(len as f64 - 1.0) }
            } else {
                len as f64 - 1.0
            };
            if from < 0.0 {
                return Ok(None);
            }
            Box::new((0..=from as usize).rev())
        } else {
            let from = relative_index(vm, &c.arg(1), len, 0)?;
            Box::new(from..len)
        };
        for i in indices {
            let v = get_at(vm, o, i)?;
            let hit = match (&v, zero) {
                (Some(v), true) => v.same_value_zero(&target),
                (None, true) => target.is_undefined(),
                (Some(v), false) => v.strict_equals(&target),
                (None, false) => false,
            };
            if hit {
                return Ok(Some(i));
            }
        }
        Ok(None)
    })
}

fn index_of(vm: &mut Vm, c: &NativeCall) -> R {
    Ok(Value::Number(search(vm, c, false, false)?.map_or(-1.0, |i| i as f64)))
}

fn last_index_of(vm: &mut Vm, c: &NativeCall) -> R {
    Ok(Value::Number(search(vm, c, true, false)?.map_or(-1.0, |i| i as f64)))
}

fn includes(vm: &mut Vm, c: &NativeCall) -> R {
    Ok(Value::Bool(search(vm, c, false, true)?.is_some()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    ForEach,
    Map,
    Filter,
    Some,
    Every,
    Find,
    FindIndex,
    FindLast,
    FindLastIndex,
}

fn iterate(vm: &mut Vm, c: &NativeCall, op: Op, name: &str) -> R {
    let o = vm.to_object(&c.this)?;
    let f = c.arg(0);
    if !vm.is_callable(&f) {
        let s = crate::inspect::inspect_short(vm, &f);
        return Err(vm.type_error(format!("{s} is not a function (Array.prototype.{name} callback)")));
    }
    let this_arg = c.arg(1);
    rooted(vm, &[Value::Object(o)], |vm| {
        let len = len_of(vm, o)?;
        let out = match op {
            Op::Map => Some(vm.new_array(vec![Value::Hole; len])),
            Op::Filter => Some(vm.new_array(Vec::new())),
            _ => None,
        };
        if let Some(a) = out {
            vm.push_root(Value::Object(a));
        }
        let finding = matches!(op, Op::Find | Op::FindIndex | Op::FindLast | Op::FindLastIndex);
        let order: Box<dyn Iterator<Item = usize>> =
            if matches!(op, Op::FindLast | Op::FindLastIndex) { Box::new((0..len).rev()) } else { Box::new(0..len) };
        for i in order {
            let v = match get_at(vm, o, i)? {
                Some(v) => v,
                None if finding => Value::Undefined,
                None => continue,
            };
            let r = vm.call(&f, this_arg.clone(), vec![v.clone(), Value::Number(i as f64), Value::Object(o)])?;
            match op {
                Op::ForEach => {}
                Op::Map => {
                    let a = out.unwrap();
                    let els = els_mut(vm, a);
                    if i < els.len() {
                        els[i] = r;
                    }
                }
                Op::Filter => {
                    if r.truthy() {
                        els_mut(vm, out.unwrap()).push(v);
                    }
                }
                Op::Some if r.truthy() => return Ok(Value::Bool(true)),
                Op::Every if !r.truthy() => return Ok(Value::Bool(false)),
                Op::Find | Op::FindLast if r.truthy() => return Ok(v),
                Op::FindIndex | Op::FindLastIndex if r.truthy() => return Ok(Value::Number(i as f64)),
                _ => {}
            }
        }
        Ok(match op {
            Op::Map | Op::Filter => Value::Object(out.unwrap()),
            Op::Some => Value::Bool(false),
            Op::Every => Value::Bool(true),
            Op::FindIndex | Op::FindLastIndex => Value::Number(-1.0),
            _ => Value::Undefined,
        })
    })
}

fn for_each(vm: &mut Vm, c: &NativeCall) -> R {
    iterate(vm, c, Op::ForEach, "forEach")
}
fn map(vm: &mut Vm, c: &NativeCall) -> R {
    iterate(vm, c, Op::Map, "map")
}
fn filter(vm: &mut Vm, c: &NativeCall) -> R {
    iterate(vm, c, Op::Filter, "filter")
}
fn some(vm: &mut Vm, c: &NativeCall) -> R {
    iterate(vm, c, Op::Some, "some")
}
fn every(vm: &mut Vm, c: &NativeCall) -> R {
    iterate(vm, c, Op::Every, "every")
}
fn find(vm: &mut Vm, c: &NativeCall) -> R {
    iterate(vm, c, Op::Find, "find")
}
fn find_index(vm: &mut Vm, c: &NativeCall) -> R {
    iterate(vm, c, Op::FindIndex, "findIndex")
}
fn find_last(vm: &mut Vm, c: &NativeCall) -> R {
    iterate(vm, c, Op::FindLast, "findLast")
}
fn find_last_index(vm: &mut Vm, c: &NativeCall) -> R {
    iterate(vm, c, Op::FindLastIndex, "findLastIndex")
}

fn reduce_impl(vm: &mut Vm, c: &NativeCall, right: bool) -> R {
    let o = vm.to_object(&c.this)?;
    let f = c.arg(0);
    require_callable(vm, &f)?;
    rooted(vm, &[Value::Object(o)], |vm| {
        let len = len_of(vm, o)?;
        let order: Vec<usize> = if right { (0..len).rev().collect() } else { (0..len).collect() };
        let mut it = order.into_iter();
        let mut acc = if c.args.len() >= 2 {
            c.arg(1)
        } else {
            loop {
                match it.next() {
                    None => return Err(vm.type_error("Reduce of empty array with no initial value")),
                    Some(i) => {
                        if let Some(v) = get_at(vm, o, i)? {
                            break v;
                        }
                    }
                }
            }
        };
        let mark = vm.root_mark();
        for i in it {
            if let Some(v) = get_at(vm, o, i)? {
                vm.pop_roots(mark);
                vm.push_root(acc.clone());
                acc = vm.call(&f, Value::Undefined, vec![acc, v, Value::Number(i as f64), Value::Object(o)])?;
            }
        }
        Ok(acc)
    })
}

fn reduce(vm: &mut Vm, c: &NativeCall) -> R {
    reduce_impl(vm, c, false)
}

fn reduce_right(vm: &mut Vm, c: &NativeCall) -> R {
    reduce_impl(vm, c, true)
}

/// Stable merge sort with a fallible comparator (`Ok(true)` = a > b).
fn merge_sort(vm: &mut Vm, v: Vec<Value>, gt: &mut dyn FnMut(&mut Vm, &Value, &Value) -> Result<bool, Value>) -> Result<Vec<Value>, Value> {
    if v.len() <= 1 {
        return Ok(v);
    }
    let mut v = v;
    let right = v.split_off(v.len() / 2);
    let left = merge_sort(vm, v, gt)?;
    let right = merge_sort(vm, right, gt)?;
    let mut out = Vec::with_capacity(left.len() + right.len());
    let (mut l, mut r) = (left.into_iter().peekable(), right.into_iter().peekable());
    while let (Some(a), Some(b)) = (l.peek(), r.peek()) {
        if gt(vm, a, b)? {
            out.push(r.next().unwrap());
        } else {
            out.push(l.next().unwrap());
        }
    }
    out.extend(l);
    out.extend(r);
    Ok(out)
}

fn check_comparator(vm: &mut Vm, cmp: &Value) -> Result<(), Value> {
    if !cmp.is_undefined() && !vm.is_callable(cmp) {
        return Err(vm.type_error("The comparison function must be either a function or undefined"));
    }
    Ok(())
}

fn sort(vm: &mut Vm, c: &NativeCall) -> R {
    let cmp = c.arg(0);
    check_comparator(vm, &cmp)?;
    let o = this_array(vm, c, "sort", false)?;
    sort_in_place(vm, o, cmp)
}

/// `this` copied into a fresh dense array (holes read as `undefined`).
fn copy_of_this(vm: &mut Vm, c: &NativeCall) -> Result<ObjRef, Value> {
    let o = vm.to_object(&c.this)?;
    let mark = vm.push_root(Value::Object(o));
    let els = list_from_array_like(vm, &Value::Object(o));
    vm.pop_roots(mark);
    let els = els?;
    Ok(vm.new_array(els))
}

fn to_sorted(vm: &mut Vm, c: &NativeCall) -> R {
    let cmp = c.arg(0);
    check_comparator(vm, &cmp)?;
    let o = copy_of_this(vm, c)?;
    rooted(vm, &[Value::Object(o)], |vm| sort_in_place(vm, o, cmp))
}

fn to_reversed(vm: &mut Vm, c: &NativeCall) -> R {
    let o = copy_of_this(vm, c)?;
    els_mut(vm, o).reverse();
    Ok(Value::Object(o))
}

fn with(vm: &mut Vm, c: &NativeCall) -> R {
    let o = copy_of_this(vm, c)?;
    rooted(vm, &[Value::Object(o)], |vm| {
        let len = els_mut(vm, o).len() as f64;
        let n = vm.to_integer(&c.arg(0))?;
        let i = if n < 0.0 { len + n } else { n };
        if i < 0.0 || i >= len {
            return Err(vm.range_error(format!("Invalid index : {}", crate::inspect::format_number(n))));
        }
        els_mut(vm, o)[i as usize] = c.arg(1);
        Ok(Value::Object(o))
    })
}

fn copy_within(vm: &mut Vm, c: &NativeCall) -> R {
    let o = this_array(vm, c, "copyWithin", false)?;
    let len = els_mut(vm, o).len();
    let target = relative_index(vm, &c.arg(0), len, 0)?;
    let start = relative_index(vm, &c.arg(1), len, 0)?;
    let end = relative_index(vm, &c.arg(2), len, len)?;
    // Conversions may have run user code that shrank the array.
    let els = els_mut(vm, o);
    let len = els.len();
    let count = end.min(len).saturating_sub(start).min(len.saturating_sub(target));
    let src: Vec<Value> = els[start.min(len)..start.min(len) + count].to_vec();
    els[target..target + count].clone_from_slice(&src);
    Ok(Value::Object(o))
}

fn sort_in_place(vm: &mut Vm, o: ObjRef, cmp: Value) -> R {
    let els = els_mut(vm, o).clone();
    let total = els.len();
    let values: Vec<Value> = els.into_iter().filter(|v| !matches!(v, Value::Hole | Value::Undefined)).collect();
    let undefs = total - values.len() - els_mut(vm, o).iter().filter(|v| matches!(v, Value::Hole)).count();
    let mark = vm.root_mark();
    for v in &values {
        vm.push_root(v.clone());
    }
    let mut gt = |vm: &mut Vm, a: &Value, b: &Value| -> Result<bool, Value> {
        if cmp.is_undefined() {
            let (x, y) = (vm.to_string(a)?, vm.to_string(b)?);
            Ok(x.units() > y.units())
        } else {
            let r = vm.call(&cmp, Value::Undefined, vec![a.clone(), b.clone()])?;
            Ok(vm.to_number(&r)? > 0.0)
        }
    };
    let sorted = merge_sort(vm, values, &mut gt);
    vm.pop_roots(mark);
    let mut sorted = sorted?;
    sorted.extend(std::iter::repeat_n(Value::Undefined, undefs));
    let els = els_mut(vm, o);
    let n = els.len().max(sorted.len());
    sorted.resize(n, Value::Hole);
    *els = sorted;
    Ok(Value::Object(o))
}

fn fill(vm: &mut Vm, c: &NativeCall) -> R {
    let o = this_array(vm, c, "fill", false)?;
    let len = els_mut(vm, o).len();
    let start = relative_index(vm, &c.arg(1), len, 0)?;
    let end = relative_index(vm, &c.arg(2), len, len)?;
    let els = els_mut(vm, o);
    for i in start..end.min(els.len()) {
        els[i] = c.arg(0);
    }
    Ok(Value::Object(o))
}

fn make_iter(vm: &mut Vm, c: &NativeCall, kind: IterKind) -> R {
    let o = vm.to_object(&c.this)?;
    let proto = Some(vm.realm.array_iter_proto);
    Ok(Value::Object(vm.new_object_with(proto, ObjectKind::ArrayIter { target: Value::Object(o), index: 0, kind, done: false })))
}

fn keys(vm: &mut Vm, c: &NativeCall) -> R {
    make_iter(vm, c, IterKind::Keys)
}
fn values(vm: &mut Vm, c: &NativeCall) -> R {
    make_iter(vm, c, IterKind::Values)
}
fn entries(vm: &mut Vm, c: &NativeCall) -> R {
    make_iter(vm, c, IterKind::Entries)
}

fn flatten_into(vm: &Vm, out: &mut Vec<Value>, els: &[Value], depth: f64) {
    for v in els {
        match v {
            Value::Hole => {}
            Value::Object(o) if depth >= 1.0 && vm.heap.obj(*o).is_array() => {
                if let ObjectKind::Array(inner) = &vm.heap.obj(*o).kind {
                    flatten_into(vm, out, inner, depth - 1.0);
                }
            }
            v => out.push(v.clone()),
        }
    }
}

fn flat(vm: &mut Vm, c: &NativeCall) -> R {
    let o = vm.to_object(&c.this)?;
    let depth = if c.arg(0).is_undefined() { 1.0 } else { vm.to_integer(&c.arg(0))? };
    let mark = vm.push_root(Value::Object(o));
    let els = list_from_array_like(vm, &Value::Object(o));
    vm.pop_roots(mark);
    let mut out = Vec::new();
    flatten_into(vm, &mut out, &els?, depth);
    Ok(new_array_from(vm, out))
}

fn flat_map(vm: &mut Vm, c: &NativeCall) -> R {
    let mapped = iterate(vm, c, Op::Map, "flatMap")?;
    let Value::Object(m) = mapped else { unreachable!() };
    let els = els_mut(vm, m).clone();
    let mut out = Vec::new();
    flatten_into(vm, &mut out, &els, 1.0);
    Ok(new_array_from(vm, out))
}

fn at(vm: &mut Vm, c: &NativeCall) -> R {
    let o = vm.to_object(&c.this)?;
    rooted(vm, &[Value::Object(o)], |vm| {
        let len = len_of(vm, o)? as f64;
        let n = vm.to_integer(&c.arg(0))?;
        let i = if n < 0.0 { len + n } else { n };
        if i < 0.0 || i >= len {
            return Ok(Value::Undefined);
        }
        Ok(get_at(vm, o, i as usize)?.unwrap_or(Value::Undefined))
    })
}

/// One step of an array iterator; `None` when exhausted.
pub(crate) fn array_iter_step(vm: &mut Vm, it: ObjRef) -> Result<Option<Value>, Value> {
    let (target, index, kind) = match &vm.heap.obj(it).kind {
        ObjectKind::ArrayIter { done: true, .. } => return Ok(None),
        ObjectKind::ArrayIter { target, index, kind, .. } => (target.clone(), *index as usize, *kind),
        _ => return Err(vm.type_error("next method called on incompatible receiver")),
    };
    let Value::Object(t) = target else { return Ok(None) };
    let len = len_of(vm, t)?;
    let set = |vm: &mut Vm, idx: u32, done: bool| {
        if let ObjectKind::ArrayIter { index, done: d, .. } = &mut vm.heap.obj_mut(it).kind {
            *index = idx;
            *d = done;
        }
    };
    if index >= len {
        set(vm, index as u32, true);
        return Ok(None);
    }
    set(vm, index as u32 + 1, false);
    Ok(Some(match kind {
        IterKind::Keys => Value::Number(index as f64),
        IterKind::Values => get_at(vm, t, index)?.unwrap_or(Value::Undefined),
        IterKind::Entries => {
            let v = get_at(vm, t, index)?.unwrap_or(Value::Undefined);
            new_array_from(vm, vec![Value::Number(index as f64), v])
        }
    }))
}

fn array_iter_next(vm: &mut Vm, c: &NativeCall) -> R {
    let Value::Object(it) = c.this else { return Err(vm.type_error("next method called on incompatible receiver")) };
    match array_iter_step(vm, it)? {
        Some(v) => Ok(iter_result(vm, v, false)),
        None => Ok(iter_result(vm, Value::Undefined, true)),
    }
}
