//! Map and Set (SameValueZero keys, insertion order, live iteration).

use super::*;

pub(crate) fn setup(vm: &mut Vm) {
    let tag = vm.realm.sym_to_string_tag.clone();
    let sym_it = vm.realm.sym_iterator.clone();

    let mp = vm.realm.map_proto;
    make_ctor(vm, "Map", 0, map_ctor, mp);
    for (name, len, f) in [
        ("get", 1, map_get as NativeFn),
        ("set", 2, map_set),
        ("has", 1, coll_has),
        ("delete", 1, coll_delete),
        ("clear", 0, coll_clear),
        ("forEach", 1, coll_for_each),
        ("keys", 0, coll_keys),
        ("values", 0, coll_values),
    ] {
        vm.define_method(mp, name, len, f);
    }
    let entries = vm.define_method(mp, "entries", 0, coll_entries);
    vm.define_value(mp, PropKey::Sym(sym_it.clone()), Value::Object(entries), HIDDEN);
    vm.define_getter(mp, "size", coll_size);
    vm.define_value(mp, PropKey::Sym(tag.clone()), Value::str("Map"), CONFIGURABLE);

    let sp = vm.realm.set_proto;
    make_ctor(vm, "Set", 0, set_ctor, sp);
    for (name, len, f) in [
        ("add", 1, set_add as NativeFn),
        ("has", 1, coll_has),
        ("delete", 1, coll_delete),
        ("clear", 0, coll_clear),
        ("forEach", 1, coll_for_each),
        ("entries", 0, coll_entries),
    ] {
        vm.define_method(sp, name, len, f);
    }
    let values = vm.define_method(sp, "values", 0, coll_values);
    vm.define_value(sp, "keys", Value::Object(values), HIDDEN);
    vm.define_value(sp, PropKey::Sym(sym_it), Value::Object(values), HIDDEN);
    vm.define_getter(sp, "size", coll_size);
    vm.define_value(sp, PropKey::Sym(tag.clone()), Value::str("Set"), CONFIGURABLE);

    for (proto, name) in [(vm.realm.map_iter_proto, "Map Iterator"), (vm.realm.set_iter_proto, "Set Iterator")] {
        vm.define_method(proto, "next", 0, coll_iter_next);
        vm.define_value(proto, PropKey::Sym(tag.clone()), Value::str(name), CONFIGURABLE);
    }
}

fn data_mut(vm: &mut Vm, o: ObjRef) -> &mut MapData {
    match &mut vm.heap.obj_mut(o).kind {
        ObjectKind::Map(m) | ObjectKind::Set(m) => m,
        _ => unreachable!(),
    }
}

fn this_coll(vm: &mut Vm, c: &NativeCall, set: Option<bool>, method: &str) -> Result<ObjRef, Value> {
    if let Value::Object(o) = c.this {
        let ok = match (&vm.heap.obj(o).kind, set) {
            (ObjectKind::Map(_), Some(false) | None) => true,
            (ObjectKind::Set(_), Some(true) | None) => true,
            _ => false,
        };
        if ok {
            return Ok(o);
        }
    }
    let what = match set {
        Some(true) => "Set",
        Some(false) => "Map",
        None => "Map/Set",
    };
    Err(vm.type_error(format!("Method {what}.prototype.{method} called on incompatible receiver")))
}

fn construct_coll(vm: &mut Vm, c: &NativeCall, is_set: bool) -> R {
    let name = if is_set { "Set" } else { "Map" };
    if !c.is_construct() {
        return Err(vm.type_error(format!("Constructor {name} requires 'new'")));
    }
    let default = if is_set { vm.realm.set_proto } else { vm.realm.map_proto };
    let proto = proto_from_new_target(vm, &c.new_target, default)?;
    let kind = if is_set { ObjectKind::Set(MapData::default()) } else { ObjectKind::Map(MapData::default()) };
    let o = vm.new_object_with(Some(proto), kind);
    let init = c.arg(0);
    if init.is_nullish() {
        return Ok(Value::Object(o));
    }
    let mark = vm.push_root(Value::Object(o));
    let r = (|| {
        let items = vm.iterate_to_vec(&init)?;
        for it in &items {
            vm.push_root(it.clone());
        }
        for it in items {
            if is_set {
                data_mut(vm, o).set(it, Value::Undefined);
            } else {
                if !matches!(it, Value::Object(_)) {
                    let s = crate::inspect::inspect_short(vm, &it);
                    return Err(vm.type_error(format!("Iterator value {s} is not an entry object")));
                }
                let k = vm.get(&it, &PropKey::str("0"))?;
                vm.push_root(k.clone());
                let v = vm.get(&it, &PropKey::str("1"))?;
                data_mut(vm, o).set(k, v);
            }
        }
        Ok(Value::Object(o))
    })();
    vm.pop_roots(mark);
    r
}

fn map_ctor(vm: &mut Vm, c: &NativeCall) -> R {
    construct_coll(vm, c, false)
}

fn set_ctor(vm: &mut Vm, c: &NativeCall) -> R {
    construct_coll(vm, c, true)
}

fn map_get(vm: &mut Vm, c: &NativeCall) -> R {
    let o = this_coll(vm, c, Some(false), "get")?;
    Ok(data_mut(vm, o).get(&c.arg(0)).cloned().unwrap_or(Value::Undefined))
}

fn map_set(vm: &mut Vm, c: &NativeCall) -> R {
    let o = this_coll(vm, c, Some(false), "set")?;
    data_mut(vm, o).set(c.arg(0), c.arg(1));
    Ok(c.this.clone())
}

fn set_add(vm: &mut Vm, c: &NativeCall) -> R {
    let o = this_coll(vm, c, Some(true), "add")?;
    data_mut(vm, o).set(c.arg(0), Value::Undefined);
    Ok(c.this.clone())
}

fn coll_has(vm: &mut Vm, c: &NativeCall) -> R {
    let o = this_coll(vm, c, None, "has")?;
    Ok(Value::Bool(data_mut(vm, o).has(&c.arg(0))))
}

fn coll_delete(vm: &mut Vm, c: &NativeCall) -> R {
    let o = this_coll(vm, c, None, "delete")?;
    Ok(Value::Bool(data_mut(vm, o).delete(&c.arg(0))))
}

fn coll_clear(vm: &mut Vm, c: &NativeCall) -> R {
    let o = this_coll(vm, c, None, "clear")?;
    data_mut(vm, o).clear();
    Ok(Value::Undefined)
}

fn coll_size(vm: &mut Vm, c: &NativeCall) -> R {
    let o = this_coll(vm, c, None, "size")?;
    Ok(Value::Number(data_mut(vm, o).size as f64))
}

fn coll_for_each(vm: &mut Vm, c: &NativeCall) -> R {
    let o = this_coll(vm, c, None, "forEach")?;
    let f = c.arg(0);
    require_callable(vm, &f)?;
    let is_set = matches!(vm.heap.obj(o).kind, ObjectKind::Set(_));
    let mut i = 0;
    loop {
        // Entries added during iteration are visited; deleted ones are skipped.
        let entry = {
            let d = data_mut(vm, o);
            if i >= d.entries.len() {
                break;
            }
            d.entries[i].clone()
        };
        i += 1;
        if let Some((k, v)) = entry {
            let first = if is_set { k.clone() } else { v };
            vm.call(&f, c.arg(1), vec![first, k, c.this.clone()])?;
        }
    }
    Ok(Value::Undefined)
}

fn make_iter(vm: &mut Vm, c: &NativeCall, kind: IterKind, method: &str) -> R {
    let o = this_coll(vm, c, None, method)?;
    let proto = if matches!(vm.heap.obj(o).kind, ObjectKind::Set(_)) { vm.realm.set_iter_proto } else { vm.realm.map_iter_proto };
    Ok(Value::Object(vm.new_object_with(Some(proto), ObjectKind::CollectionIter { coll: o, index: 0, kind, done: false })))
}

fn coll_keys(vm: &mut Vm, c: &NativeCall) -> R {
    make_iter(vm, c, IterKind::Keys, "keys")
}

fn coll_values(vm: &mut Vm, c: &NativeCall) -> R {
    make_iter(vm, c, IterKind::Values, "values")
}

fn coll_entries(vm: &mut Vm, c: &NativeCall) -> R {
    make_iter(vm, c, IterKind::Entries, "entries")
}

fn coll_iter_next(vm: &mut Vm, c: &NativeCall) -> R {
    let Value::Object(it) = c.this else { return Err(vm.type_error("next method called on incompatible receiver")) };
    let (coll, mut index, kind, done) = match &vm.heap.obj(it).kind {
        ObjectKind::CollectionIter { coll, index, kind, done } => (*coll, *index, *kind, *done),
        _ => return Err(vm.type_error("next method called on incompatible receiver")),
    };
    if done {
        return Ok(iter_result(vm, Value::Undefined, true));
    }
    let is_set = matches!(vm.heap.obj(coll).kind, ObjectKind::Set(_));
    let found = loop {
        let d = data_mut(vm, coll);
        if index >= d.entries.len() {
            break None;
        }
        index += 1;
        if let Some(e) = &d.entries[index - 1] {
            break Some(e.clone());
        }
    };
    if let ObjectKind::CollectionIter { index: i, done: d, .. } = &mut vm.heap.obj_mut(it).kind {
        *i = index;
        *d = found.is_none();
    }
    let Some((k, v)) = found else { return Ok(iter_result(vm, Value::Undefined, true)) };
    let value = match kind {
        IterKind::Keys => k,
        IterKind::Values if is_set => k,
        IterKind::Values => v,
        IterKind::Entries => {
            let second = if is_set { k.clone() } else { v };
            Value::Object(vm.new_array(vec![k, second]))
        }
    };
    Ok(iter_result(vm, value, false))
}
