//! JavaScript values.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use crate::heap::ObjRef;

/// Immutable UTF-16 string (JavaScript string semantics: lengths and
/// indices count UTF-16 code units; lone surrogates are representable).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct JsStr(Rc<[u16]>);

impl JsStr {
    pub fn new(s: &str) -> Self {
        JsStr(s.encode_utf16().collect::<Vec<_>>().into())
    }

    pub fn from_units(v: Vec<u16>) -> Self {
        JsStr(v.into())
    }

    pub fn units(&self) -> &[u16] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn concat(&self, other: &JsStr) -> JsStr {
        if other.is_empty() {
            return self.clone();
        }
        if self.is_empty() {
            return other.clone();
        }
        let mut v = Vec::with_capacity(self.len() + other.len());
        v.extend_from_slice(&self.0);
        v.extend_from_slice(&other.0);
        JsStr(v.into())
    }

    /// Lossy conversion (lone surrogates become U+FFFD).
    pub fn to_rust(&self) -> String {
        String::from_utf16_lossy(&self.0)
    }

    pub fn eq_str(&self, s: &str) -> bool {
        self.0.iter().copied().eq(s.encode_utf16())
    }

    /// Canonical array index (`"0"`, `"17"`, never `"01"`), below 2^32 - 1.
    pub fn as_array_index(&self) -> Option<u32> {
        let u = &self.0;
        if u.is_empty() || u.len() > 10 {
            return None;
        }
        if u[0] == b'0' as u16 {
            return if u.len() == 1 { Some(0) } else { None };
        }
        let mut n: u64 = 0;
        for &c in u.iter() {
            if !(b'0' as u16..=b'9' as u16).contains(&c) {
                return None;
            }
            n = n * 10 + (c - b'0' as u16) as u64;
        }
        if n < u32::MAX as u64 { Some(n as u32) } else { None }
    }
}

impl fmt::Debug for JsStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}", self.to_rust())
    }
}

impl fmt::Display for JsStr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_rust())
    }
}

impl From<&str> for JsStr {
    fn from(s: &str) -> Self {
        JsStr::new(s)
    }
}

/// A symbol: identity is the allocation.
#[derive(Clone)]
pub struct Symbol(pub Rc<SymbolData>);

pub struct SymbolData {
    pub description: Option<JsStr>,
}

impl Symbol {
    pub fn new(description: Option<JsStr>) -> Self {
        Symbol(Rc::new(SymbolData { description }))
    }

    pub fn description(&self) -> Option<&JsStr> {
        self.0.description.as_ref()
    }
}

impl PartialEq for Symbol {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Symbol {}

impl Hash for Symbol {
    fn hash<H: Hasher>(&self, state: &mut H) {
        (Rc::as_ptr(&self.0) as usize).hash(state)
    }
}

impl fmt::Debug for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Symbol({})", self.description().map(|d| d.to_rust()).unwrap_or_default())
    }
}

#[derive(Clone, Debug, Default)]
pub enum Value {
    #[default]
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(JsStr),
    Symbol(Symbol),
    Object(ObjRef),
    /// Internal: uninitialized binding (temporal dead zone). Never observable.
    Hole,
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::String(JsStr::new(s))
    }

    pub fn is_undefined(&self) -> bool {
        matches!(self, Value::Undefined)
    }

    pub fn is_nullish(&self) -> bool {
        matches!(self, Value::Undefined | Value::Null)
    }

    pub fn as_object(&self) -> Option<ObjRef> {
        match self {
            Value::Object(o) => Some(*o),
            _ => None,
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Value::Undefined | Value::Null | Value::Hole => false,
            Value::Bool(b) => *b,
            Value::Number(n) => !(*n == 0.0 || n.is_nan()),
            Value::String(s) => !s.is_empty(),
            Value::Symbol(_) | Value::Object(_) => true,
        }
    }

    /// SameValue (Object.is).
    pub fn same_value(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => {
                (a.is_nan() && b.is_nan()) || (a == b && a.is_sign_negative() == b.is_sign_negative())
            }
            _ => self.strict_equals(other),
        }
    }

    /// SameValueZero (Map/Set keys, Array.prototype.includes).
    pub fn same_value_zero(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Number(a), Value::Number(b)) => (a.is_nan() && b.is_nan()) || a == b,
            _ => self.strict_equals(other),
        }
    }

    /// `===`
    pub fn strict_equals(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Undefined, Value::Undefined) | (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Number(a), Value::Number(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Symbol(a), Value::Symbol(b)) => a == b,
            (Value::Object(a), Value::Object(b)) => a == b,
            _ => false,
        }
    }
}

/// Property key: string or symbol.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum PropKey {
    Str(JsStr),
    Sym(Symbol),
}

impl PropKey {
    pub fn str(s: &str) -> PropKey {
        PropKey::Str(JsStr::new(s))
    }

    pub fn as_index(&self) -> Option<u32> {
        match self {
            PropKey::Str(s) => s.as_array_index(),
            PropKey::Sym(_) => None,
        }
    }

    pub fn to_value(&self) -> Value {
        match self {
            PropKey::Str(s) => Value::String(s.clone()),
            PropKey::Sym(s) => Value::Symbol(s.clone()),
        }
    }

    pub fn is_symbol(&self) -> bool {
        matches!(self, PropKey::Sym(_))
    }

    pub fn display(&self) -> String {
        match self {
            PropKey::Str(s) => s.to_rust(),
            PropKey::Sym(s) => format!("Symbol({})", s.description().map(|d| d.to_rust()).unwrap_or_default()),
        }
    }
}

impl From<&str> for PropKey {
    fn from(s: &str) -> Self {
        PropKey::str(s)
    }
}

/// Hashable SameValueZero key for Map / Set.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum MapKey {
    Undefined,
    Null,
    Bool(bool),
    Number(u64),
    String(JsStr),
    Symbol(Symbol),
    Object(ObjRef),
}

impl MapKey {
    pub fn from_value(v: &Value) -> MapKey {
        match v {
            Value::Undefined | Value::Hole => MapKey::Undefined,
            Value::Null => MapKey::Null,
            Value::Bool(b) => MapKey::Bool(*b),
            Value::Number(n) => {
                let n = if *n == 0.0 { 0.0 } else { *n };
                MapKey::Number(if n.is_nan() { f64::NAN.to_bits() } else { n.to_bits() })
            }
            Value::String(s) => MapKey::String(s.clone()),
            Value::Symbol(s) => MapKey::Symbol(s.clone()),
            Value::Object(o) => MapKey::Object(*o),
        }
    }
}
