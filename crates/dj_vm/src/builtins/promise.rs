//! Promise, timers, queueMicrotask.

use super::*;

pub(crate) fn setup(vm: &mut Vm) {
    let pp = vm.realm.promise_proto;
    let ctor = make_ctor(vm, "Promise", 1, promise_ctor, pp);
    vm.realm.promise_ctor = Some(ctor);
    vm.define_method(pp, "then", 2, then);
    vm.define_method(pp, "catch", 1, catch);
    vm.define_method(pp, "finally", 1, finally);
    let tag = vm.realm.sym_to_string_tag.clone();
    vm.define_value(pp, PropKey::Sym(tag), Value::str("Promise"), CONFIGURABLE);
    vm.define_method(ctor, "resolve", 1, promise_resolve);
    vm.define_method(ctor, "reject", 1, promise_reject);
    vm.define_method(ctor, "all", 1, promise_all);
    vm.define_method(ctor, "allSettled", 1, promise_all_settled);
    vm.define_method(ctor, "any", 1, promise_any);
    vm.define_method(ctor, "race", 1, promise_race);

    let g = vm.realm.global;
    vm.define_method(g, "setTimeout", 2, set_timeout);
    vm.define_method(g, "setInterval", 2, set_interval);
    vm.define_method(g, "clearTimeout", 1, clear_timer);
    vm.define_method(g, "clearInterval", 1, clear_timer);
    vm.define_method(g, "queueMicrotask", 1, queue_microtask);
}

fn data(vm: &Vm, f: ObjRef) -> Vec<Value> {
    match &vm.heap.obj(f).kind {
        ObjectKind::Native(n) => n.data.clone(),
        _ => Vec::new(),
    }
}

fn is_promise(vm: &Vm, v: &Value) -> Option<ObjRef> {
    match v {
        Value::Object(o) if matches!(vm.heap.obj(*o).kind, ObjectKind::Promise(_)) => Some(*o),
        _ => None,
    }
}

/// CreateResolvingFunctions: `(resolve, reject)` sharing an "already resolved" flag.
pub(crate) fn resolving_functions(vm: &mut Vm, p: ObjRef) -> (ObjRef, ObjRef) {
    let flag = vm.alloc_cell(Value::Bool(false));
    let d = vec![Value::Object(p), Value::Object(flag)];
    let res = vm.new_native_full("", 1, resolve_function, d.clone(), false);
    let rej = vm.new_native_full("", 1, reject_function, d, false);
    (res, rej)
}

fn take_flag(vm: &mut Vm, d: &[Value]) -> Option<ObjRef> {
    let (Value::Object(p), Value::Object(flag)) = (&d[0], &d[1]) else { return None };
    let cell = vm.heap.cell_mut(*flag);
    if cell.truthy() {
        return None;
    }
    *cell = Value::Bool(true);
    Some(*p)
}

fn resolve_function(vm: &mut Vm, c: &NativeCall) -> R {
    let d = data(vm, c.callee);
    if let Some(p) = take_flag(vm, &d) {
        vm.resolve_promise(p, c.arg(0));
    }
    Ok(Value::Undefined)
}

fn reject_function(vm: &mut Vm, c: &NativeCall) -> R {
    let d = data(vm, c.callee);
    if let Some(p) = take_flag(vm, &d) {
        vm.reject_promise(p, c.arg(0));
    }
    Ok(Value::Undefined)
}

fn promise_ctor(vm: &mut Vm, c: &NativeCall) -> R {
    if !c.is_construct() {
        return Err(vm.type_error("Promise constructor cannot be invoked without 'new'"));
    }
    let executor = c.arg(0);
    if !vm.is_callable(&executor) {
        let s = crate::inspect::inspect_short(vm, &executor);
        return Err(vm.type_error(format!("Promise resolver {s} is not a function")));
    }
    let pp = vm.realm.promise_proto;
    let proto = proto_from_new_target(vm, &c.new_target, pp)?;
    let p = vm.new_promise();
    vm.heap.obj_mut(p).proto = Some(proto);
    let mark = vm.push_root(Value::Object(p));
    let (res, rej) = resolving_functions(vm, p);
    vm.push_root(Value::Object(res));
    vm.push_root(Value::Object(rej));
    let r = vm.call(&executor, Value::Undefined, vec![Value::Object(res), Value::Object(rej)]);
    let out = match r {
        Ok(_) => Ok(Value::Object(p)),
        Err(e) if vm.terminating => Err(e),
        Err(e) => vm.call(&Value::Object(rej), Value::Undefined, vec![e]).map(|_| Value::Object(p)),
    };
    vm.pop_roots(mark);
    out
}

fn reaction(vm: &Vm, v: &Value) -> ReactionKind {
    ReactionKind::Handler(if vm.is_callable(v) { v.clone() } else { Value::Undefined })
}

fn then(vm: &mut Vm, c: &NativeCall) -> R {
    let Some(p) = is_promise(vm, &c.this) else {
        return Err(vm.type_error("Method Promise.prototype.then called on incompatible receiver"));
    };
    let derived = vm.new_promise();
    let (f, r) = (reaction(vm, &c.arg(0)), reaction(vm, &c.arg(1)));
    vm.perform_then(p, f, r, Some(derived));
    Ok(Value::Object(derived))
}

fn invoke_then(vm: &mut Vm, p: &Value, args: Vec<Value>) -> R {
    let t = vm.get(p, &PropKey::str("then"))?;
    vm.call(&t, p.clone(), args)
}

fn catch(vm: &mut Vm, c: &NativeCall) -> R {
    invoke_then(vm, &c.this, vec![Value::Undefined, c.arg(0)])
}

fn finally(vm: &mut Vm, c: &NativeCall) -> R {
    let on_finally = c.arg(0);
    if !vm.is_callable(&on_finally) {
        return invoke_then(vm, &c.this, vec![on_finally.clone(), on_finally]);
    }
    let f = vm.new_native_full("", 1, then_finally, vec![on_finally.clone(), Value::Bool(false)], false);
    let mark = vm.push_root(Value::Object(f));
    let r = vm.new_native_full("", 1, then_finally, vec![on_finally, Value::Bool(true)], false);
    vm.push_root(Value::Object(r));
    let out = invoke_then(vm, &c.this, vec![Value::Object(f), Value::Object(r)]);
    vm.pop_roots(mark);
    out
}

/// `finally` reaction: run the callback, then pass the original outcome through.
fn then_finally(vm: &mut Vm, c: &NativeCall) -> R {
    let d = data(vm, c.callee);
    let result = vm.call(&d[0], Value::Undefined, Vec::new())?;
    let p = vm.promise_resolve_value(result);
    let mark = vm.push_root(Value::Object(p));
    let thunk = vm.new_native_full("", 0, value_thunk, vec![c.arg(0), d[1].clone()], false);
    vm.push_root(Value::Object(thunk));
    let out = invoke_then(vm, &Value::Object(p), vec![Value::Object(thunk)]);
    vm.pop_roots(mark);
    out
}

fn value_thunk(vm: &mut Vm, c: &NativeCall) -> R {
    let d = data(vm, c.callee);
    if d[1].truthy() { Err(d[0].clone()) } else { Ok(d[0].clone()) }
}

fn promise_resolve(vm: &mut Vm, c: &NativeCall) -> R {
    let v = c.arg(0);
    if let Some(p) = is_promise(vm, &v) {
        let ctor = vm.get(&v, &PropKey::str("constructor"))?;
        if ctor.strict_equals(&c.this) {
            return Ok(Value::Object(p));
        }
    }
    let p = vm.new_promise();
    let mark = vm.push_root(Value::Object(p));
    vm.resolve_promise(p, v);
    vm.pop_roots(mark);
    Ok(Value::Object(p))
}

fn promise_reject(vm: &mut Vm, c: &NativeCall) -> R {
    let p = vm.new_promise();
    vm.reject_promise(p, c.arg(0));
    Ok(Value::Object(p))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Combinator {
    All,
    AllSettled,
    Any,
}

// Element function data: [index, values array, remaining cell, result promise, called cell, kind, rejected?]
fn combinator(vm: &mut Vm, c: &NativeCall, kind: Combinator) -> R {
    let result = vm.new_promise();
    let mark = vm.push_root(Value::Object(result));
    let r = (|| {
        let items = vm.iterate_to_vec(&c.arg(0))?;
        let values = vm.new_array(vec![Value::Undefined; items.len()]);
        vm.push_root(Value::Object(values));
        let remaining = vm.alloc_cell(Value::Number(1.0));
        vm.push_root(Value::Object(remaining));
        for v in &items {
            vm.push_root(v.clone());
        }
        let (res_all, rej_all) = resolving_functions(vm, result);
        vm.push_root(Value::Object(res_all));
        vm.push_root(Value::Object(rej_all));
        for (i, item) in items.into_iter().enumerate() {
            let p = vm.promise_resolve_value(item);
            vm.push_root(Value::Object(p));
            if let Value::Number(n) = vm.heap.cell(remaining) {
                let n = *n + 1.0;
                *vm.heap.cell_mut(remaining) = Value::Number(n);
            }
            let make = |vm: &mut Vm, rejected: bool| {
                let called = vm.alloc_cell(Value::Bool(false));
                let d = vec![
                    Value::Number(i as f64),
                    Value::Object(values),
                    Value::Object(remaining),
                    Value::Object(result),
                    Value::Object(called),
                    Value::Number(kind as u8 as f64),
                    Value::Bool(rejected),
                ];
                ReactionKind::Handler(Value::Object(vm.new_native_full("", 1, element_function, d, false)))
            };
            let (on_ok, on_err) = match kind {
                Combinator::All => (make(vm, false), ReactionKind::Handler(Value::Object(rej_all))),
                Combinator::AllSettled => (make(vm, false), make(vm, true)),
                Combinator::Any => (ReactionKind::Handler(Value::Object(res_all)), make(vm, true)),
            };
            vm.perform_then(p, on_ok, on_err, None);
        }
        finish_element(vm, values, remaining, result, kind);
        Ok(Value::Object(result))
    })();
    vm.pop_roots(mark);
    match r {
        Ok(v) => Ok(v),
        Err(e) => {
            vm.reject_promise(result, e);
            Ok(Value::Object(result))
        }
    }
}

/// Decrement the remaining count; settle the result promise at zero.
fn finish_element(vm: &mut Vm, values: ObjRef, remaining: ObjRef, result: ObjRef, kind: Combinator) {
    let n = match vm.heap.cell(remaining) {
        Value::Number(n) => *n - 1.0,
        _ => 0.0,
    };
    *vm.heap.cell_mut(remaining) = Value::Number(n);
    if n != 0.0 {
        return;
    }
    if kind == Combinator::Any {
        let errs = vm.new_array(match &vm.heap.obj(values).kind {
            ObjectKind::Array(e) => e.clone(),
            _ => Vec::new(),
        });
        let e = vm.make_error(ErrorType::Error, "All promises were rejected");
        let eo = e.as_object().unwrap();
        vm.define_value(eo, "name", Value::str("AggregateError"), HIDDEN);
        vm.define_value(eo, "errors", Value::Object(errs), HIDDEN);
        vm.reject_promise(result, e);
    } else {
        vm.resolve_promise(result, Value::Object(values));
    }
}

fn element_function(vm: &mut Vm, c: &NativeCall) -> R {
    let d = data(vm, c.callee);
    let called = d[4].as_object().unwrap();
    if vm.heap.cell(called).truthy() {
        return Ok(Value::Undefined);
    }
    *vm.heap.cell_mut(called) = Value::Bool(true);
    let (i, values, remaining, result) = match (&d[0], &d[1], &d[2], &d[3]) {
        (Value::Number(i), Value::Object(v), Value::Object(r), Value::Object(p)) => (*i as usize, *v, *r, *p),
        _ => unreachable!(),
    };
    let kind = match d[5] {
        Value::Number(k) if k == 0.0 => Combinator::All,
        Value::Number(k) if k == 1.0 => Combinator::AllSettled,
        _ => Combinator::Any,
    };
    let rejected = d[6].truthy();
    let v = if kind == Combinator::AllSettled {
        let o = vm.new_object();
        let (status, key) = if rejected { ("rejected", "reason") } else { ("fulfilled", "value") };
        vm.define_value(o, "status", Value::str(status), DEFAULT_FLAGS);
        vm.define_value(o, key, c.arg(0), DEFAULT_FLAGS);
        Value::Object(o)
    } else {
        c.arg(0)
    };
    if let ObjectKind::Array(els) = &mut vm.heap.obj_mut(values).kind {
        if i < els.len() {
            els[i] = v;
        }
    }
    finish_element(vm, values, remaining, result, kind);
    Ok(Value::Undefined)
}

fn promise_all(vm: &mut Vm, c: &NativeCall) -> R {
    combinator(vm, c, Combinator::All)
}

fn promise_all_settled(vm: &mut Vm, c: &NativeCall) -> R {
    combinator(vm, c, Combinator::AllSettled)
}

fn promise_any(vm: &mut Vm, c: &NativeCall) -> R {
    combinator(vm, c, Combinator::Any)
}

fn promise_race(vm: &mut Vm, c: &NativeCall) -> R {
    let result = vm.new_promise();
    let mark = vm.push_root(Value::Object(result));
    let r = (|| {
        let items = vm.iterate_to_vec(&c.arg(0))?;
        for v in &items {
            vm.push_root(v.clone());
        }
        let (res, rej) = resolving_functions(vm, result);
        vm.push_root(Value::Object(res));
        vm.push_root(Value::Object(rej));
        for item in items {
            let p = vm.promise_resolve_value(item);
            vm.perform_then(p, ReactionKind::Handler(Value::Object(res)), ReactionKind::Handler(Value::Object(rej)), None);
        }
        Ok(())
    })();
    vm.pop_roots(mark);
    if let Err(e) = r {
        vm.reject_promise(result, e);
    }
    Ok(Value::Object(result))
}

// --- timers ------------------------------------------------------------------------

fn timer(vm: &mut Vm, c: &NativeCall, repeat: bool) -> R {
    let cb = c.arg(0);
    if !vm.is_callable(&cb) {
        let s = crate::inspect::inspect_short(vm, &cb);
        return Err(vm.type_error(format!("The \"callback\" argument must be of type function. Received {s}")));
    }
    let delay = vm.to_number(&c.arg(1))?;
    let args = c.args.iter().skip(2).cloned().collect();
    Ok(Value::Number(vm.add_timer(cb, delay, args, repeat) as f64))
}

fn set_timeout(vm: &mut Vm, c: &NativeCall) -> R {
    timer(vm, c, false)
}

fn set_interval(vm: &mut Vm, c: &NativeCall) -> R {
    timer(vm, c, true)
}

fn clear_timer(vm: &mut Vm, c: &NativeCall) -> R {
    if let Value::Number(n) = c.arg(0) {
        vm.clear_timer(n as u64);
    }
    Ok(Value::Undefined)
}

fn queue_microtask(vm: &mut Vm, c: &NativeCall) -> R {
    let f = c.arg(0);
    require_callable(vm, &f)?;
    vm.enqueue_callback(f);
    Ok(Value::Undefined)
}
