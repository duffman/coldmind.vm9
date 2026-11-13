//! Object model.

use std::collections::HashMap;

use indexmap::IndexMap;

use crate::heap::ObjRef;
use crate::value::{JsStr, MapKey, PropKey, Symbol, Value};
use crate::vm::Vm;

pub const WRITABLE: u8 = 1;
pub const ENUMERABLE: u8 = 2;
pub const CONFIGURABLE: u8 = 4;
/// Ordinary assignment-created property.
pub const DEFAULT_FLAGS: u8 = WRITABLE | ENUMERABLE | CONFIGURABLE;
/// Builtin methods, class methods, `constructor`.
pub const HIDDEN: u8 = WRITABLE | CONFIGURABLE;

#[derive(Clone, Debug)]
pub enum Slot {
    Data(Value),
    Accessor { get: Option<ObjRef>, set: Option<ObjRef> },
}

#[derive(Clone, Debug)]
pub struct Property {
    pub slot: Slot,
    pub flags: u8,
}

impl Property {
    pub fn data(v: Value, flags: u8) -> Self {
        Property { slot: Slot::Data(v), flags }
    }

    pub fn writable(&self) -> bool {
        self.flags & WRITABLE != 0
    }

    pub fn enumerable(&self) -> bool {
        self.flags & ENUMERABLE != 0
    }

    pub fn configurable(&self) -> bool {
        self.flags & CONFIGURABLE != 0
    }
}

pub type PropMap = IndexMap<PropKey, Property>;

/// Native function signature. Natives that call back into JavaScript must
/// root any heap value they hold in Rust locals across that call
/// (`Vm::push_root`); arguments and `this` are rooted by the caller.
pub type NativeFn = fn(&mut Vm, &NativeCall) -> Result<Value, Value>;

pub struct NativeCall {
    pub this: Value,
    pub args: Vec<Value>,
    /// `undefined` for plain calls; the constructor for `new`.
    pub new_target: Value,
    pub callee: ObjRef,
}

impl NativeCall {
    pub fn arg(&self, i: usize) -> Value {
        self.args.get(i).cloned().unwrap_or(Value::Undefined)
    }

    pub fn is_construct(&self) -> bool {
        !self.new_target.is_undefined()
    }
}

#[derive(Debug)]
pub struct Closure {
    pub module: u32,
    pub func: u32,
    /// Captured cells.
    pub upvalues: Box<[ObjRef]>,
    pub home_object: Option<ObjRef>,
    pub fields_init: Option<ObjRef>,
}

pub struct NativeFunction {
    pub f: NativeFn,
    /// Captured values (bound arguments, resolving-function state, ...).
    pub data: Vec<Value>,
    pub constructor: bool,
    pub name: JsStr,
}

#[derive(Clone, Debug)]
pub enum PromiseState {
    Pending,
    Fulfilled(Value),
    Rejected(Value),
}

#[derive(Clone, Debug)]
pub enum ReactionKind {
    /// Call the handler (or pass the value through when `undefined`).
    Handler(Value),
    /// Resume a suspended async function.
    Resume(ObjRef),
}

#[derive(Clone, Debug)]
pub struct Reaction {
    pub kind: ReactionKind,
    /// Promise resolved with the handler's result.
    pub target: Option<ObjRef>,
}

#[derive(Debug)]
pub struct PromiseData {
    pub state: PromiseState,
    pub fulfill_reactions: Vec<Reaction>,
    pub reject_reactions: Vec<Reaction>,
    /// A rejection handler was attached at some point.
    pub handled: bool,
}

/// Insertion-ordered hash table for Map and Set (Set stores `undefined` values).
#[derive(Default, Debug)]
pub struct MapData {
    pub entries: Vec<Option<(Value, Value)>>,
    pub index: HashMap<MapKey, usize>,
    pub size: usize,
}

impl MapData {
    pub fn get(&self, k: &Value) -> Option<&Value> {
        self.index.get(&MapKey::from_value(k)).and_then(|&i| self.entries[i].as_ref().map(|e| &e.1))
    }

    pub fn has(&self, k: &Value) -> bool {
        self.index.contains_key(&MapKey::from_value(k))
    }

    pub fn set(&mut self, k: Value, v: Value) {
        let key = MapKey::from_value(&k);
        if let Some(&i) = self.index.get(&key) {
            if let Some(e) = &mut self.entries[i] {
                e.1 = v;
            }
            return;
        }
        // Normalize -0 to +0 as the spec requires for keys.
        let k = match k {
            Value::Number(n) if n == 0.0 => Value::Number(0.0),
            k => k,
        };
        self.index.insert(key, self.entries.len());
        self.entries.push(Some((k, v)));
        self.size += 1;
    }

    pub fn delete(&mut self, k: &Value) -> bool {
        match self.index.remove(&MapKey::from_value(k)) {
            Some(i) => {
                self.entries[i] = None;
                self.size -= 1;
                true
            }
            None => false,
        }
    }

    /// Entries stay in place (tombstoned) so live iterators keep their position.
    pub fn clear(&mut self) {
        for e in &mut self.entries {
            *e = None;
        }
        self.index.clear();
        self.size = 0;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IterKind {
    Keys,
    Values,
    Entries,
}

pub enum ObjectKind {
    Ordinary,
    /// Dense elements; `Value::Hole` marks holes.
    Array(Vec<Value>),
    Closure(Closure),
    Native(NativeFunction),
    Error,
    BooleanBox(bool),
    NumberBox(f64),
    StringBox(JsStr),
    SymbolBox(Symbol),
    Promise(PromiseData),
    Map(MapData),
    Set(MapData),
    ArrayIter { target: Value, index: u32, kind: IterKind, done: bool },
    StringIter { s: JsStr, pos: usize },
    /// Iterator over a Map or Set.
    CollectionIter { coll: ObjRef, index: usize, kind: IterKind, done: bool },
    ForInIter { obj: Option<ObjRef>, keys: Vec<JsStr>, pos: usize },
}

impl ObjectKind {
    pub fn class_name(&self) -> &'static str {
        match self {
            ObjectKind::Ordinary => "Object",
            ObjectKind::Array(_) => "Array",
            ObjectKind::Closure(_) | ObjectKind::Native(_) => "Function",
            ObjectKind::Error => "Error",
            ObjectKind::BooleanBox(_) => "Boolean",
            ObjectKind::NumberBox(_) => "Number",
            ObjectKind::StringBox(_) => "String",
            ObjectKind::SymbolBox(_) => "Symbol",
            ObjectKind::Promise(_) => "Promise",
            ObjectKind::Map(_) => "Map",
            ObjectKind::Set(_) => "Set",
            ObjectKind::ArrayIter { .. } => "Array Iterator",
            ObjectKind::StringIter { .. } => "String Iterator",
            ObjectKind::CollectionIter { .. } => "Iterator",
            ObjectKind::ForInIter { .. } => "ForIn Iterator",
        }
    }
}

pub struct JsObject {
    pub proto: Option<ObjRef>,
    pub props: PropMap,
    pub kind: ObjectKind,
    pub extensible: bool,
    /// Array elements are read-only and non-configurable (`Object.freeze`).
    pub elements_frozen: bool,
}

impl JsObject {
    pub fn new(proto: Option<ObjRef>, kind: ObjectKind) -> Self {
        JsObject { proto, props: PropMap::default(), kind, extensible: true, elements_frozen: false }
    }

    pub fn is_callable(&self) -> bool {
        matches!(self.kind, ObjectKind::Closure(_) | ObjectKind::Native(_))
    }

    pub fn is_array(&self) -> bool {
        matches!(self.kind, ObjectKind::Array(_))
    }
}
