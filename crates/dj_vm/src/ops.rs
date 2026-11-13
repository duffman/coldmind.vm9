//! Abstract operations (ECMAScript §7) and object internal methods.

use crate::heap::ObjRef;
use crate::numconv::{number_to_string, string_to_number, to_uint32};
use crate::object::*;
use crate::value::{JsStr, PropKey, Value};
use crate::vm::Vm;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hint {
    Default,
    Number,
    String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Access {
    Get,
    Set,
    Delete,
}

/// Largest array the dense element store accepts.
pub const MAX_DENSE_LENGTH: u32 = 1 << 26;

impl Vm {
    // --- type conversion -------------------------------------------------------

    pub fn type_of(&self, v: &Value) -> &'static str {
        match v {
            Value::Undefined | Value::Hole => "undefined",
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Symbol(_) => "symbol",
            Value::Object(o) => {
                if self.heap.obj(*o).is_callable() {
                    "function"
                } else {
                    "object"
                }
            }
        }
    }

    pub fn to_primitive(&mut self, v: &Value, hint: Hint) -> Result<Value, Value> {
        let Value::Object(o) = v else { return Ok(v.clone()) };
        let order = if hint == Hint::String { ["toString", "valueOf"] } else { ["valueOf", "toString"] };
        for name in order {
            let m = self.get_with_receiver(*o, &PropKey::str(name), v)?;
            if self.is_callable(&m) {
                let r = self.call(&m, v.clone(), Vec::new())?;
                if !matches!(r, Value::Object(_)) {
                    return Ok(r);
                }
            }
        }
        Err(self.type_error("Cannot convert object to primitive value"))
    }

    pub fn to_number(&mut self, v: &Value) -> Result<f64, Value> {
        Ok(match v {
            Value::Undefined | Value::Hole => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => *b as u8 as f64,
            Value::Number(n) => *n,
            Value::String(s) => string_to_number(s.units()),
            Value::Symbol(_) => return Err(self.type_error("Cannot convert a Symbol value to a number")),
            Value::Object(_) => {
                let p = self.to_primitive(v, Hint::Number)?;
                return self.to_number(&p);
            }
        })
    }

    pub fn to_string(&mut self, v: &Value) -> Result<JsStr, Value> {
        Ok(match v {
            Value::Undefined | Value::Hole => JsStr::new("undefined"),
            Value::Null => JsStr::new("null"),
            Value::Bool(b) => JsStr::new(if *b { "true" } else { "false" }),
            Value::Number(n) => JsStr::new(&number_to_string(*n)),
            Value::String(s) => s.clone(),
            Value::Symbol(_) => return Err(self.type_error("Cannot convert a Symbol value to a string")),
            Value::Object(_) => {
                let p = self.to_primitive(v, Hint::String)?;
                return self.to_string(&p);
            }
        })
    }

    pub fn to_property_key(&mut self, v: &Value) -> Result<PropKey, Value> {
        Ok(match v {
            Value::String(s) => PropKey::Str(s.clone()),
            Value::Symbol(s) => PropKey::Sym(s.clone()),
            Value::Object(_) => {
                let p = self.to_primitive(v, Hint::String)?;
                return self.to_property_key(&p);
            }
            other => PropKey::Str(self.to_string(other)?),
        })
    }

    fn key_preview(v: &Value) -> String {
        match v {
            Value::String(s) => s.to_rust(),
            Value::Number(n) => number_to_string(*n),
            Value::Symbol(s) => format!("Symbol({})", s.description().map(|d| d.to_rust()).unwrap_or_default()),
            Value::Bool(b) => b.to_string(),
            Value::Null => "null".into(),
            Value::Undefined | Value::Hole => "undefined".into(),
            Value::Object(_) => "[object]".into(),
        }
    }

    pub(crate) fn nullish_access_error(&mut self, base: &Value, key: &str, access: Access) -> Value {
        let what = if matches!(base, Value::Null) { "null" } else { "undefined" };
        match access {
            Access::Get => self.type_error(format!("Cannot read properties of {what} (reading '{key}')")),
            Access::Set => self.type_error(format!("Cannot set properties of {what} (setting '{key}')")),
            Access::Delete => self.type_error(format!("Cannot convert undefined or null to object")),
        }
    }

    /// ToPropertyKey with the base's RequireObjectCoercible check first.
    pub(crate) fn to_property_key_for(&mut self, base: &Value, key: &Value) -> Result<PropKey, Value> {
        if base.is_nullish() {
            let k = Self::key_preview(key);
            return Err(self.nullish_access_error(base, &k, Access::Get));
        }
        self.to_property_key(key)
    }

    pub fn to_object(&mut self, v: &Value) -> Result<ObjRef, Value> {
        let (proto, kind) = match v {
            Value::Object(o) => return Ok(*o),
            Value::Undefined | Value::Null | Value::Hole => return Err(self.type_error("Cannot convert undefined or null to object")),
            Value::Bool(b) => (self.realm.boolean_proto, ObjectKind::BooleanBox(*b)),
            Value::Number(n) => (self.realm.number_proto, ObjectKind::NumberBox(*n)),
            Value::String(s) => (self.realm.string_proto, ObjectKind::StringBox(s.clone())),
            Value::Symbol(s) => (self.realm.symbol_proto, ObjectKind::SymbolBox(s.clone())),
        };
        Ok(self.new_object_with(Some(proto), kind))
    }

    /// ToIntegerOrInfinity
    pub fn to_integer(&mut self, v: &Value) -> Result<f64, Value> {
        let n = self.to_number(v)?;
        Ok(crate::numconv::to_integer(n))
    }

    /// ToLength
    pub fn to_length(&mut self, v: &Value) -> Result<f64, Value> {
        let n = self.to_integer(v)?;
        Ok(n.clamp(0.0, 9007199254740991.0))
    }

    // --- own properties --------------------------------------------------------

    /// GetOwnProperty, including virtual array / string elements and `length`.
    pub fn get_own_property(&self, o: ObjRef, key: &PropKey) -> Option<Property> {
        let obj = self.heap.obj(o);
        match &obj.kind {
            ObjectKind::Array(els) => {
                if let PropKey::Str(s) = key {
                    if let Some(i) = s.as_array_index() {
                        return match els.get(i as usize) {
                            Some(Value::Hole) | None => None,
                            Some(v) => Some(Property::data(
                                v.clone(),
                                if obj.elements_frozen { ENUMERABLE } else { DEFAULT_FLAGS },
                            )),
                        };
                    }
                    if s.eq_str("length") {
                        let w = if obj.elements_frozen { 0 } else { WRITABLE };
                        return Some(Property::data(Value::Number(els.len() as f64), w));
                    }
                }
            }
            ObjectKind::StringBox(st) => {
                if let PropKey::Str(s) = key {
                    if let Some(i) = s.as_array_index() {
                        if (i as usize) < st.len() {
                            let c = JsStr::from_units(vec![st.units()[i as usize]]);
                            return Some(Property::data(Value::String(c), ENUMERABLE));
                        }
                    }
                    if s.eq_str("length") {
                        return Some(Property::data(Value::Number(st.len() as f64), 0));
                    }
                }
            }
            _ => {}
        }
        obj.props.get(key).cloned()
    }

    pub fn has_own_property(&self, o: ObjRef, key: &PropKey) -> bool {
        self.get_own_property(o, key).is_some()
    }

    pub fn has_property(&mut self, o: ObjRef, key: &PropKey) -> Result<bool, Value> {
        let mut cur = Some(o);
        while let Some(c) = cur {
            if self.has_own_property(c, key) {
                return Ok(true);
            }
            cur = self.heap.obj(c).proto;
        }
        Ok(false)
    }

    /// OwnPropertyKeys order: integer indices ascending, strings in
    /// insertion order, then symbols in insertion order.
    pub fn own_keys(&self, o: ObjRef) -> Vec<PropKey> {
        let obj = self.heap.obj(o);
        let mut ints: Vec<u32> = Vec::new();
        match &obj.kind {
            ObjectKind::Array(els) => {
                ints.extend(els.iter().enumerate().filter(|(_, v)| !matches!(v, Value::Hole)).map(|(i, _)| i as u32))
            }
            ObjectKind::StringBox(s) => ints.extend(0..s.len() as u32),
            _ => {}
        }
        let mut strs = Vec::new();
        let mut syms = Vec::new();
        for k in obj.props.keys() {
            match k {
                PropKey::Str(s) => match s.as_array_index() {
                    Some(i) => ints.push(i),
                    None => strs.push(k.clone()),
                },
                PropKey::Sym(_) => syms.push(k.clone()),
            }
        }
        ints.sort_unstable();
        ints.dedup();
        let mut out: Vec<PropKey> = ints.into_iter().map(|i| PropKey::Str(JsStr::new(&i.to_string()))).collect();
        if matches!(obj.kind, ObjectKind::Array(_)) {
            out.push(PropKey::str("length"));
        } else if matches!(obj.kind, ObjectKind::StringBox(_)) {
            out.push(PropKey::str("length"));
        }
        out.extend(strs);
        out.extend(syms);
        out
    }

    /// Own enumerable string-keyed property names, in order.
    pub fn own_enumerable_string_keys(&self, o: ObjRef) -> Vec<JsStr> {
        self.own_keys(o)
            .into_iter()
            .filter_map(|k| match &k {
                PropKey::Str(s) => self.get_own_property(o, &k).filter(|p| p.enumerable()).map(|_| s.clone()),
                PropKey::Sym(_) => None,
            })
            .collect()
    }

    pub fn is_array(&self, v: &Value) -> bool {
        matches!(v, Value::Object(o) if self.heap.obj(*o).is_array())
    }

    // --- [[Get]] / [[Set]] -------------------------------------------------------

    pub fn get(&mut self, target: &Value, key: &PropKey) -> Result<Value, Value> {
        let proto = match target {
            Value::Object(o) => return self.get_with_receiver(*o, key, target),
            Value::String(s) => {
                if let PropKey::Str(k) = key {
                    if let Some(i) = k.as_array_index() {
                        if (i as usize) < s.len() {
                            return Ok(Value::String(JsStr::from_units(vec![s.units()[i as usize]])));
                        }
                    } else if k.eq_str("length") {
                        return Ok(Value::Number(s.len() as f64));
                    }
                }
                self.realm.string_proto
            }
            Value::Number(_) => self.realm.number_proto,
            Value::Bool(_) => self.realm.boolean_proto,
            Value::Symbol(_) => self.realm.symbol_proto,
            Value::Undefined | Value::Null | Value::Hole => {
                let k = key.display();
                return Err(self.nullish_access_error(target, &k, Access::Get));
            }
        };
        self.get_with_receiver(proto, key, target)
    }

    pub fn get_str(&mut self, target: &Value, key: &str) -> Result<Value, Value> {
        self.get(target, &PropKey::str(key))
    }

    pub fn get_with_receiver(&mut self, o: ObjRef, key: &PropKey, receiver: &Value) -> Result<Value, Value> {
        let mut cur = Some(o);
        while let Some(c) = cur {
            // Ordinary data properties: avoid cloning the whole property.
            let obj = self.heap.obj(c);
            if matches!(obj.kind, ObjectKind::Ordinary | ObjectKind::Closure(_) | ObjectKind::Native(_) | ObjectKind::Error) {
                match obj.props.get(key) {
                    Some(Property { slot: Slot::Data(v), .. }) => return Ok(v.clone()),
                    Some(Property { slot: Slot::Accessor { get, .. }, .. }) => {
                        let g = *get;
                        return match g {
                            Some(g) => self.call(&Value::Object(g), receiver.clone(), Vec::new()),
                            None => Ok(Value::Undefined),
                        };
                    }
                    None => {
                        cur = obj.proto;
                        continue;
                    }
                }
            }
            match self.get_own_property(c, key) {
                Some(Property { slot: Slot::Data(v), .. }) => return Ok(v),
                Some(Property { slot: Slot::Accessor { get, .. }, .. }) => {
                    return match get {
                        Some(g) => self.call(&Value::Object(g), receiver.clone(), Vec::new()),
                        None => Ok(Value::Undefined),
                    };
                }
                None => cur = self.heap.obj(c).proto,
            }
        }
        Ok(Value::Undefined)
    }

    pub fn set(&mut self, target: &Value, key: PropKey, v: Value) -> Result<(), Value> {
        match target {
            Value::Object(o) => self.set_with_receiver(*o, key, v, target),
            Value::Undefined | Value::Null | Value::Hole => {
                let k = key.display();
                Err(self.nullish_access_error(target, &k, Access::Set))
            }
            prim => {
                // Setters on the prototype chain still run; plain writes fail (strict mode).
                let proto = match prim {
                    Value::String(_) => self.realm.string_proto,
                    Value::Number(_) => self.realm.number_proto,
                    Value::Bool(_) => self.realm.boolean_proto,
                    _ => self.realm.symbol_proto,
                };
                let mut cur = Some(proto);
                while let Some(c) = cur {
                    if let Some(Property { slot: Slot::Accessor { set: Some(s), .. }, .. }) = self.get_own_property(c, &key) {
                        self.call(&Value::Object(s), prim.clone(), vec![v])?;
                        return Ok(());
                    }
                    cur = self.heap.obj(c).proto;
                }
                let p = crate::inspect::inspect_short(self, prim);
                Err(self.type_error(format!("Cannot create property '{}' on {} {p}", key.display(), self.type_of(prim))))
            }
        }
    }

    pub fn set_str(&mut self, target: &Value, key: &str, v: Value) -> Result<(), Value> {
        self.set(target, PropKey::str(key), v)
    }

    /// OrdinarySet (strict mode: failures throw).
    pub fn set_with_receiver(&mut self, o: ObjRef, key: PropKey, v: Value, receiver: &Value) -> Result<(), Value> {
        let mut cur = Some(o);
        while let Some(c) = cur {
            match self.get_own_property(c, &key) {
                Some(Property { slot: Slot::Data(_), flags }) => {
                    if flags & WRITABLE == 0 {
                        return Err(self.type_error(format!("Cannot assign to read only property '{}' of object", key.display())));
                    }
                    break;
                }
                Some(Property { slot: Slot::Accessor { set, .. }, .. }) => {
                    return match set {
                        Some(s) => {
                            self.call(&Value::Object(s), receiver.clone(), vec![v])?;
                            Ok(())
                        }
                        None => Err(self.type_error(format!(
                            "Cannot set property {} of {} which has only a getter",
                            key.display(),
                            crate::inspect::inspect_short(self, receiver)
                        ))),
                    };
                }
                None => cur = self.heap.obj(c).proto,
            }
        }
        let Value::Object(r) = receiver else {
            return Err(self.type_error(format!("Cannot create property '{}' on primitive", key.display())));
        };
        self.put_own(*r, key, v)
    }

    /// Write an own data property (create if absent).
    fn put_own(&mut self, r: ObjRef, key: PropKey, v: Value) -> Result<(), Value> {
        let is_array = self.heap.obj(r).is_array();
        if is_array {
            if let PropKey::Str(s) = &key {
                if let Some(i) = s.as_array_index() {
                    return self.set_array_element(r, i, v);
                }
                if s.eq_str("length") {
                    return self.set_array_length(r, &v);
                }
            }
        }
        let obj = self.heap.obj_mut(r);
        if let Some(p) = obj.props.get_mut(&key) {
            if let Slot::Data(slot) = &mut p.slot {
                *slot = v;
                return Ok(());
            }
        }
        if !obj.extensible {
            return Err(self.type_error(format!("Cannot add property {}, object is not extensible", key.display())));
        }
        obj.props.insert(key, Property::data(v, DEFAULT_FLAGS));
        Ok(())
    }

    pub fn set_array_element(&mut self, a: ObjRef, i: u32, v: Value) -> Result<(), Value> {
        let obj = self.heap.obj_mut(a);
        let (frozen, extensible) = (obj.elements_frozen, obj.extensible);
        let ObjectKind::Array(els) = &mut obj.kind else { unreachable!() };
        let i = i as usize;
        if i < els.len() && !matches!(els[i], Value::Hole) {
            if frozen {
                return Err(self.type_error(format!("Cannot assign to read only property '{i}' of object")));
            }
            els[i] = v;
            return Ok(());
        }
        if !extensible {
            return Err(self.type_error(format!("Cannot add property {i}, object is not extensible")));
        }
        if i >= els.len() {
            if i as u64 >= MAX_DENSE_LENGTH as u64 {
                return Err(self.range_error(format!("Array index {i} exceeds the maximum dense array length")));
            }
            els.resize(i, Value::Hole);
            els.push(v);
        } else {
            els[i] = v;
        }
        Ok(())
    }

    pub fn set_array_length(&mut self, a: ObjRef, v: &Value) -> Result<(), Value> {
        let n = self.to_number(v)?;
        if n != to_uint32(n) as f64 {
            return Err(self.range_error("Invalid array length"));
        }
        if n as u64 > MAX_DENSE_LENGTH as u64 {
            return Err(self.range_error("Invalid array length (exceeds the maximum dense array length)"));
        }
        let obj = self.heap.obj_mut(a);
        if obj.elements_frozen {
            return Err(self.type_error("Cannot assign to read only property 'length' of object"));
        }
        if let ObjectKind::Array(els) = &mut obj.kind {
            els.resize(n as usize, Value::Hole);
        }
        Ok(())
    }

    /// CreateDataPropertyOrThrow.
    pub fn create_data_property(&mut self, o: ObjRef, key: PropKey, v: Value) -> Result<(), Value> {
        if self.heap.obj(o).is_array() {
            if let Some(i) = key.as_index() {
                return self.set_array_element(o, i, v);
            }
        }
        let obj = self.heap.obj_mut(o);
        if let Some(p) = obj.props.get(&key) {
            if !p.configurable() {
                return Err(self.type_error(format!("Cannot redefine property: {}", key.display())));
            }
        } else if !obj.extensible {
            return Err(self.type_error(format!("Cannot define property {}, object is not extensible", key.display())));
        }
        obj.props.insert(key, Property::data(v, DEFAULT_FLAGS));
        Ok(())
    }

    /// Define (or replace) a property with explicit attributes. Array
    /// elements and `length` only accept plain writable data descriptors.
    pub fn define_own_property(&mut self, o: ObjRef, key: PropKey, prop: Property) -> Result<(), Value> {
        if self.heap.obj(o).is_array() {
            if let PropKey::Str(s) = &key {
                if s.as_array_index().is_some() || s.eq_str("length") {
                    if let Slot::Data(v) = &prop.slot {
                        if prop.flags & WRITABLE != 0 || s.eq_str("length") {
                            let v = v.clone();
                            return if s.eq_str("length") { self.set_array_length(o, &v) } else { self.set_array_element(o, s.as_array_index().unwrap(), v) };
                        }
                    }
                    return Err(self.type_error("Accessor or read-only array elements are not supported"));
                }
            }
        }
        let obj = self.heap.obj_mut(o);
        match obj.props.get(&key) {
            Some(p) if !p.configurable() => {
                // Only a writable data property's value may change.
                let ok = matches!((&p.slot, &prop.slot), (Slot::Data(_), Slot::Data(_))) && p.writable() && prop.flags == p.flags & !0;
                if !ok {
                    return Err(self.type_error(format!("Cannot redefine property: {}", key.display())));
                }
            }
            None if !obj.extensible => {
                return Err(self.type_error(format!("Cannot define property {}, object is not extensible", key.display())));
            }
            _ => {}
        }
        obj.props.insert(key, prop);
        Ok(())
    }

    pub fn delete_property(&mut self, o: ObjRef, key: &PropKey) -> Result<bool, Value> {
        let obj = self.heap.obj_mut(o);
        let frozen = obj.elements_frozen;
        if let ObjectKind::Array(els) = &mut obj.kind {
            if let Some(i) = key.as_index() {
                if frozen {
                    return Ok(false);
                }
                if let Some(slot) = els.get_mut(i as usize) {
                    *slot = Value::Hole;
                }
                return Ok(true);
            }
            if matches!(key, PropKey::Str(s) if s.eq_str("length")) {
                return Ok(false);
            }
        }
        if let ObjectKind::StringBox(s) = &obj.kind {
            if key.as_index().is_some_and(|i| (i as usize) < s.len()) || matches!(key, PropKey::Str(k) if k.eq_str("length")) {
                return Ok(false);
            }
        }
        match obj.props.get(key) {
            Some(p) if !p.configurable() => Ok(false),
            Some(_) => {
                obj.props.shift_remove(key);
                Ok(true)
            }
            None => Ok(true),
        }
    }

    /// `delete v[key]` in strict mode.
    pub(crate) fn delete_property_value(&mut self, v: &Value, key: &PropKey) -> Result<bool, Value> {
        let o = match v {
            Value::Undefined | Value::Null | Value::Hole => {
                let k = key.display();
                return Err(self.nullish_access_error(v, &k, Access::Delete));
            }
            _ => self.to_object(v)?,
        };
        if self.delete_property(o, key)? {
            Ok(true)
        } else {
            Err(self.type_error(format!("Cannot delete property '{}' of {}", key.display(), crate::inspect::inspect_short(self, v))))
        }
    }

    // --- operators ---------------------------------------------------------------

    pub fn op_add(&mut self, a: &Value, b: &Value) -> Result<Value, Value> {
        let pa = self.to_primitive(a, Hint::Default)?;
        let mark = self.push_root(pa.clone());
        let pb = self.to_primitive(b, Hint::Default);
        self.pop_roots(mark);
        let pb = pb?;
        if matches!(pa, Value::String(_)) || matches!(pb, Value::String(_)) {
            let sa = self.to_string(&pa)?;
            let sb = self.to_string(&pb)?;
            return Ok(Value::String(sa.concat(&sb)));
        }
        Ok(Value::Number(self.to_number(&pa)? + self.to_number(&pb)?))
    }

    /// IsLooselyEqual (`==`).
    pub fn loose_equals(&mut self, a: &Value, b: &Value) -> Result<bool, Value> {
        use Value::*;
        Ok(match (a, b) {
            (Undefined | Null | Hole, Undefined | Null | Hole) => true,
            (Undefined | Null | Hole, _) | (_, Undefined | Null | Hole) => false,
            (Number(x), String(s)) => *x == string_to_number(s.units()),
            (String(s), Number(x)) => string_to_number(s.units()) == *x,
            (Bool(x), _) => return self.loose_equals(&Number(*x as u8 as f64), b),
            (_, Bool(y)) => return self.loose_equals(a, &Number(*y as u8 as f64)),
            (Object(_), Number(_) | String(_) | Symbol(_)) => {
                let p = self.to_primitive(a, Hint::Default)?;
                return self.loose_equals(&p, b);
            }
            (Number(_) | String(_) | Symbol(_), Object(_)) => {
                let p = self.to_primitive(b, Hint::Default)?;
                return self.loose_equals(a, &p);
            }
            _ => a.strict_equals(b),
        })
    }

    /// IsLessThan: `Some(bool)` or `None` for undefined (NaN involved).
    pub fn abstract_lt(&mut self, a: &Value, b: &Value, left_first: bool) -> Result<Option<bool>, Value> {
        let (pa, pb) = if left_first {
            let pa = self.to_primitive(a, Hint::Number)?;
            let mark = self.push_root(pa.clone());
            let pb = self.to_primitive(b, Hint::Number);
            self.pop_roots(mark);
            (pa, pb?)
        } else {
            let pb = self.to_primitive(b, Hint::Number)?;
            let mark = self.push_root(pb.clone());
            let pa = self.to_primitive(a, Hint::Number);
            self.pop_roots(mark);
            (pa?, pb)
        };
        if let (Value::String(x), Value::String(y)) = (&pa, &pb) {
            return Ok(Some(x.units() < y.units()));
        }
        let x = self.to_number(&pa)?;
        let y = self.to_number(&pb)?;
        if x.is_nan() || y.is_nan() {
            return Ok(None);
        }
        Ok(Some(x < y))
    }

    pub fn instance_of(&mut self, v: &Value, target: &Value) -> Result<bool, Value> {
        let Value::Object(t) = target else {
            let s = crate::inspect::inspect_short(self, target);
            return Err(self.type_error(format!("Right-hand side of 'instanceof' is not an object ({s})")));
        };
        let hi = self.get(target, &PropKey::Sym(self.realm.sym_has_instance.clone()))?;
        if !hi.is_nullish() {
            let r = self.call(&hi, target.clone(), vec![v.clone()])?;
            return Ok(r.truthy());
        }
        if !self.heap.obj(*t).is_callable() {
            return Err(self.type_error("Right-hand side of 'instanceof' is not callable"));
        }
        self.ordinary_has_instance(*t, v)
    }

    pub fn ordinary_has_instance(&mut self, c: ObjRef, v: &Value) -> Result<bool, Value> {
        if let Some(target) = crate::builtins::bound_target(self, c) {
            return self.ordinary_has_instance(target, v);
        }
        let Value::Object(mut o) = v.clone() else { return Ok(false) };
        let proto = self.get(&Value::Object(c), &PropKey::str("prototype"))?;
        let Value::Object(proto) = proto else {
            return Err(self.type_error("Function has non-object prototype in instanceof check"));
        };
        loop {
            match self.heap.obj(o).proto {
                Some(p) if p == proto => return Ok(true),
                Some(p) => o = p,
                None => return Ok(false),
            }
        }
    }

    // --- iteration ---------------------------------------------------------------

    pub fn get_iterator(&mut self, v: &Value) -> Result<ObjRef, Value> {
        let key = PropKey::Sym(self.realm.sym_iterator.clone());
        let m = if v.is_nullish() { Value::Undefined } else { self.get(v, &key)? };
        if !self.is_callable(&m) {
            let s = crate::inspect::inspect_short(self, v);
            return Err(self.type_error(format!("{s} is not iterable")));
        }
        let it = self.call(&m, v.clone(), Vec::new())?;
        match it {
            Value::Object(o) => Ok(o),
            _ => Err(self.type_error("Result of the Symbol.iterator method is not an object")),
        }
    }

    /// IteratorStep + IteratorValue: `None` when done.
    pub fn iterator_step(&mut self, it: ObjRef) -> Result<Option<Value>, Value> {
        // Fast paths for built-in iterators whose `next` is unmodified.
        match &self.heap.obj(it).kind {
            ObjectKind::ForInIter { .. } => return self.for_in_step(it),
            ObjectKind::ArrayIter { .. } if self.realm.array_iter_next.is_some() => {
                let next = self.get(&Value::Object(it), &PropKey::str("next"))?;
                if next.as_object() == self.realm.array_iter_next {
                    return crate::builtins::array_iter_step(self, it);
                }
            }
            _ => {}
        }
        let itv = Value::Object(it);
        let next = self.get(&itv, &PropKey::str("next"))?;
        let r = self.call(&next, itv, Vec::new())?;
        let Value::Object(_) = r else {
            let s = crate::inspect::inspect_short(self, &r);
            return Err(self.type_error(format!("Iterator result {s} is not an object")));
        };
        let mark = self.push_root(r.clone());
        let done = self.get(&r, &PropKey::str("done"));
        let out = match done {
            Ok(d) if d.truthy() => Ok(None),
            Ok(_) => self.get(&r, &PropKey::str("value")).map(Some),
            Err(e) => Err(e),
        };
        self.pop_roots(mark);
        out
    }

    pub fn iterator_close(&mut self, it: ObjRef) -> Result<(), Value> {
        let itv = Value::Object(it);
        let ret = self.get(&itv, &PropKey::str("return"))?;
        if ret.is_nullish() {
            return Ok(());
        }
        let r = self.call(&ret, itv, Vec::new())?;
        if !matches!(r, Value::Object(_)) {
            return Err(self.type_error("Iterator result is not an object"));
        }
        Ok(())
    }

    /// Exhaust an iterable into a vector (values stay rooted while iterating).
    pub fn iterate_to_vec(&mut self, v: &Value) -> Result<Vec<Value>, Value> {
        let it = self.get_iterator(v)?;
        let acc = self.new_array(Vec::new());
        let mark = self.push_root(Value::Object(it));
        self.push_root(Value::Object(acc));
        let r = loop {
            match self.iterator_step(it) {
                Ok(Some(x)) => {
                    if let ObjectKind::Array(els) = &mut self.heap.obj_mut(acc).kind {
                        els.push(x);
                    }
                }
                Ok(None) => break Ok(()),
                Err(e) => break Err(e),
            }
        };
        self.pop_roots(mark);
        r?;
        Ok(match &mut self.heap.obj_mut(acc).kind {
            ObjectKind::Array(els) => std::mem::take(els),
            _ => unreachable!(),
        })
    }

    pub(crate) fn for_in_iterator(&mut self, v: &Value) -> Result<ObjRef, Value> {
        let proto = Some(self.realm.for_in_iter_proto);
        if v.is_nullish() {
            return Ok(self.new_object_with(proto, ObjectKind::ForInIter { obj: None, keys: Vec::new(), pos: 0 }));
        }
        let o = self.to_object(v)?;
        let mut seen: std::collections::HashSet<PropKey> = Default::default();
        let mut keys = Vec::new();
        let mut cur = Some(o);
        while let Some(c) = cur {
            for k in self.own_keys(c) {
                if let PropKey::Str(s) = &k {
                    if seen.insert(k.clone()) && self.get_own_property(c, &k).is_some_and(|p| p.enumerable()) {
                        keys.push(s.clone());
                    }
                }
            }
            cur = self.heap.obj(c).proto;
        }
        Ok(self.new_object_with(proto, ObjectKind::ForInIter { obj: Some(o), keys, pos: 0 }))
    }

    fn for_in_step(&mut self, it: ObjRef) -> Result<Option<Value>, Value> {
        loop {
            let (obj, key) = match &mut self.heap.obj_mut(it).kind {
                ObjectKind::ForInIter { obj, keys, pos } => {
                    if *pos >= keys.len() {
                        return Ok(None);
                    }
                    *pos += 1;
                    (*obj, keys[*pos - 1].clone())
                }
                _ => unreachable!(),
            };
            // Keys deleted during iteration are skipped.
            if let Some(o) = obj {
                if !self.has_property(o, &PropKey::Str(key.clone()))? {
                    continue;
                }
            }
            return Ok(Some(Value::String(key)));
        }
    }

    // --- functions and classes ---------------------------------------------------

    pub(crate) fn make_class(&mut self, ctor: ObjRef, parent: Option<Value>) -> Result<(), Value> {
        let (proto_parent, ctor_parent) = match parent {
            None => (Some(self.realm.object_proto), self.realm.function_proto),
            Some(Value::Null) => (None, self.realm.function_proto),
            Some(p) => {
                if !self.is_constructor(&p) {
                    let s = crate::inspect::inspect_short(self, &p);
                    return Err(self.type_error(format!("Class extends value {s} is not a constructor or null")));
                }
                let pp = self.get(&p, &PropKey::str("prototype"))?;
                let pp = match pp {
                    Value::Object(o) => Some(o),
                    Value::Null => None,
                    _ => return Err(self.type_error("Class extends value does not have valid prototype property")),
                };
                (pp, p.as_object().unwrap())
            }
        };
        let proto = self.new_object_with(proto_parent, ObjectKind::Ordinary);
        self.define_value(proto, "constructor", Value::Object(ctor), HIDDEN);
        let c = self.heap.obj_mut(ctor);
        c.proto = Some(ctor_parent);
        c.props.insert(PropKey::str("prototype"), Property::data(Value::Object(proto), 0));
        if let ObjectKind::Closure(cl) = &mut c.kind {
            cl.home_object = Some(proto);
        }
        Ok(())
    }

    /// `DefineMethod`: flags bits 0-1 kind (0 method, 1 getter, 2 setter), bit 2 enumerable.
    pub(crate) fn define_method_property(&mut self, obj: ObjRef, key: PropKey, func: ObjRef, flags: u32) -> Result<(), Value> {
        if let ObjectKind::Closure(c) = &mut self.heap.obj_mut(func).kind {
            c.home_object = Some(obj);
        }
        let kind = flags & 3;
        let enumerable = if flags & 4 != 0 { ENUMERABLE } else { 0 };
        let prefix = match kind {
            1 => "get ",
            2 => "set ",
            _ => "",
        };
        let has_name = matches!(
            self.heap.obj(func).props.get(&PropKey::str("name")),
            Some(Property { slot: Slot::Data(Value::String(s)), .. }) if !s.is_empty()
        );
        if !has_name || !prefix.is_empty() {
            let base = match &key {
                PropKey::Str(s) => s.to_rust(),
                PropKey::Sym(s) => format!("[{}]", s.description().map(|d| d.to_rust()).unwrap_or_default()),
            };
            let name = if has_name && prefix.is_empty() { None } else { Some(format!("{prefix}{base}")) };
            if let Some(n) = name {
                self.define_value(func, "name", Value::str(&n), CONFIGURABLE);
            }
        }
        if kind == 0 {
            let p = Property::data(Value::Object(func), WRITABLE | CONFIGURABLE | enumerable);
            return self.define_own_property(obj, key, p);
        }
        let existing = self.heap.obj(obj).props.get(&key).cloned();
        let (mut get, mut set) = match existing {
            Some(Property { slot: Slot::Accessor { get, set }, .. }) => (get, set),
            _ => (None, None),
        };
        if kind == 1 {
            get = Some(func);
        } else {
            set = Some(func);
        }
        self.define_own_property(obj, key, Property { slot: Slot::Accessor { get, set }, flags: CONFIGURABLE | enumerable })
    }

    /// CopyDataProperties (object spread / rest).
    pub(crate) fn copy_data_properties(&mut self, target: ObjRef, source: &Value, excluded: &[PropKey]) -> Result<(), Value> {
        if source.is_nullish() {
            return Ok(());
        }
        let from = self.to_object(source)?;
        let mark = self.push_root(Value::Object(from));
        let r = (|| {
            for k in self.own_keys(from) {
                if excluded.contains(&k) {
                    continue;
                }
                if self.get_own_property(from, &k).is_some_and(|p| p.enumerable()) {
                    let v = self.get_with_receiver(from, &k, &Value::Object(from))?;
                    self.create_data_property(target, k, v)?;
                }
            }
            Ok(())
        })();
        self.pop_roots(mark);
        r
    }
}
