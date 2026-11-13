//! JSON.parse / JSON.stringify.

use super::*;

pub(crate) fn setup(vm: &mut Vm) {
    let json = vm.new_object();
    let g = vm.realm.global;
    vm.define_value(g, "JSON", Value::Object(json), HIDDEN);
    vm.define_method(json, "parse", 2, parse);
    vm.define_method(json, "stringify", 3, stringify);
    let tag = vm.realm.sym_to_string_tag.clone();
    vm.define_value(json, PropKey::Sym(tag), Value::str("JSON"), CONFIGURABLE);
}

// --- stringify ---------------------------------------------------------------------

pub(crate) fn quote_json(s: &[u16], out: &mut String) {
    out.push('"');
    let mut i = 0;
    while i < s.len() {
        let u = s[i];
        match u {
            0x22 => out.push_str("\\\""),
            0x5C => out.push_str("\\\\"),
            0x08 => out.push_str("\\b"),
            0x0C => out.push_str("\\f"),
            0x0A => out.push_str("\\n"),
            0x0D => out.push_str("\\r"),
            0x09 => out.push_str("\\t"),
            u if u < 0x20 => out.push_str(&format!("\\u{u:04x}")),
            0xD800..=0xDBFF if i + 1 < s.len() && (0xDC00..=0xDFFF).contains(&s[i + 1]) => {
                let cp = 0x10000 + (((u as u32) - 0xD800) << 10) + (s[i + 1] as u32 - 0xDC00);
                out.push(char::from_u32(cp).unwrap());
                i += 1;
            }
            0xD800..=0xDFFF => out.push_str(&format!("\\u{u:04x}")),
            u => out.push(char::from_u32(u as u32).unwrap()),
        }
        i += 1;
    }
    out.push('"');
}

struct Ser {
    replacer: Option<Value>,
    allow: Option<Vec<JsStr>>,
    gap: String,
    indent: String,
    stack: Vec<ObjRef>,
}

fn unwrap_box(vm: &mut Vm, v: Value) -> Result<Value, Value> {
    let Value::Object(o) = &v else { return Ok(v) };
    Ok(match &vm.heap.obj(*o).kind {
        ObjectKind::NumberBox(_) => Value::Number(vm.to_number(&v)?),
        ObjectKind::StringBox(_) => Value::String(vm.to_string(&v)?),
        ObjectKind::BooleanBox(b) => Value::Bool(*b),
        _ => v,
    })
}

fn serialize_property(vm: &mut Vm, ser: &mut Ser, holder: ObjRef, key: &JsStr) -> Result<Option<String>, Value> {
    let mark = vm.root_mark();
    let r = serialize_property_inner(vm, ser, holder, key);
    vm.pop_roots(mark);
    r
}

fn serialize_property_inner(vm: &mut Vm, ser: &mut Ser, holder: ObjRef, key: &JsStr) -> Result<Option<String>, Value> {
    let mut v = vm.get_with_receiver(holder, &PropKey::Str(key.clone()), &Value::Object(holder))?;
    vm.push_root(v.clone());
    if matches!(v, Value::Object(_)) {
        let to_json = vm.get(&v, &PropKey::str("toJSON"))?;
        if vm.is_callable(&to_json) {
            v = vm.call(&to_json, v.clone(), vec![Value::String(key.clone())])?;
            vm.push_root(v.clone());
        }
    }
    if let Some(r) = ser.replacer.clone() {
        v = vm.call(&r, Value::Object(holder), vec![Value::String(key.clone()), v])?;
        vm.push_root(v.clone());
    }
    let v = unwrap_box(vm, v)?;
    Ok(match &v {
        Value::Null => Some("null".into()),
        Value::Bool(b) => Some(b.to_string()),
        Value::String(s) => {
            let mut out = String::new();
            quote_json(s.units(), &mut out);
            Some(out)
        }
        Value::Number(n) => Some(if n.is_finite() { number_to_string(*n) } else { "null".into() }),
        Value::Object(o) if !vm.heap.obj(*o).is_callable() => {
            let o = *o;
            if ser.stack.contains(&o) {
                return Err(vm.type_error("Converting circular structure to JSON"));
            }
            ser.stack.push(o);
            let r = if vm.heap.obj(o).is_array() { serialize_array(vm, ser, o) } else { serialize_object(vm, ser, o) };
            ser.stack.pop();
            Some(r?)
        }
        _ => None,
    })
}

fn wrap(ser: &Ser, open: char, close: char, parts: Vec<String>, stepback: &str) -> String {
    if parts.is_empty() {
        return format!("{open}{close}");
    }
    if ser.gap.is_empty() {
        format!("{open}{}{close}", parts.join(","))
    } else {
        let sep = format!(",\n{}", ser.indent);
        format!("{open}\n{}{}\n{stepback}{close}", ser.indent, parts.join(&sep))
    }
}

fn serialize_object(vm: &mut Vm, ser: &mut Ser, o: ObjRef) -> Result<String, Value> {
    let stepback = ser.indent.clone();
    ser.indent.push_str(&ser.gap.clone());
    let keys = match &ser.allow {
        Some(a) => a.clone(),
        None => vm.own_enumerable_string_keys(o),
    };
    let mut parts = Vec::new();
    for k in keys {
        if let Some(s) = serialize_property(vm, ser, o, &k)? {
            let mut member = String::new();
            quote_json(k.units(), &mut member);
            member.push(':');
            if !ser.gap.is_empty() {
                member.push(' ');
            }
            member.push_str(&s);
            parts.push(member);
        }
    }
    let r = wrap(ser, '{', '}', parts, &stepback);
    ser.indent = stepback;
    Ok(r)
}

fn serialize_array(vm: &mut Vm, ser: &mut Ser, o: ObjRef) -> Result<String, Value> {
    let stepback = ser.indent.clone();
    ser.indent.push_str(&ser.gap.clone());
    let len = super::array::len_of(vm, o)?;
    let mut parts = Vec::with_capacity(len);
    for i in 0..len {
        parts.push(serialize_property(vm, ser, o, &JsStr::new(&i.to_string()))?.unwrap_or_else(|| "null".into()));
    }
    let r = wrap(ser, '[', ']', parts, &stepback);
    ser.indent = stepback;
    Ok(r)
}

fn stringify(vm: &mut Vm, c: &NativeCall) -> R {
    let replacer = c.arg(1);
    let mut ser = Ser { replacer: None, allow: None, gap: String::new(), indent: String::new(), stack: Vec::new() };
    if vm.is_callable(&replacer) {
        ser.replacer = Some(replacer.clone());
    } else if vm.is_array(&replacer) {
        let items = list_from_array_like(vm, &replacer)?;
        let mut allow: Vec<JsStr> = Vec::new();
        for it in items {
            let it = unwrap_box(vm, it)?;
            let k = match it {
                Value::String(s) => s,
                Value::Number(n) => JsStr::new(&number_to_string(n)),
                _ => continue,
            };
            if !allow.contains(&k) {
                allow.push(k);
            }
        }
        ser.allow = Some(allow);
    }
    let space = unwrap_box(vm, c.arg(2))?;
    ser.gap = match space {
        Value::Number(n) => " ".repeat(crate::numconv::to_integer(n).clamp(0.0, 10.0) as usize),
        Value::String(s) => s.to_rust().chars().take(10).collect(),
        _ => String::new(),
    };
    let wrapper = vm.new_object();
    vm.define_value(wrapper, "", c.arg(0), DEFAULT_FLAGS);
    let mark = vm.push_root(Value::Object(wrapper));
    let r = serialize_property(vm, &mut ser, wrapper, &JsStr::default());
    vm.pop_roots(mark);
    Ok(match r? {
        Some(s) => Value::str(&s),
        None => Value::Undefined,
    })
}

// --- parse -------------------------------------------------------------------------

struct Parser<'s> {
    s: &'s [u16],
    i: usize,
}

enum PErr {
    End,
    Token(usize),
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], 0x20 | 0x09 | 0x0A | 0x0D) {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u16> {
        self.s.get(self.i).copied()
    }

    fn expect(&mut self, c: u8) -> Result<(), PErr> {
        match self.peek() {
            Some(x) if x == c as u16 => {
                self.i += 1;
                Ok(())
            }
            Some(_) => Err(PErr::Token(self.i)),
            None => Err(PErr::End),
        }
    }

    fn literal(&mut self, word: &str, v: Value) -> Result<Value, PErr> {
        for b in word.bytes() {
            self.expect(b)?;
        }
        Ok(v)
    }

    fn value(&mut self, vm: &mut Vm) -> Result<Value, PErr> {
        self.ws();
        let Some(c) = self.peek() else { return Err(PErr::End) };
        match c as u8 {
            b'{' if c < 128 => {
                self.i += 1;
                let o = vm.new_object();
                self.ws();
                if self.peek() == Some(b'}' as u16) {
                    self.i += 1;
                    return Ok(Value::Object(o));
                }
                loop {
                    self.ws();
                    if self.peek() != Some(b'"' as u16) {
                        return Err(if self.peek().is_none() { PErr::End } else { PErr::Token(self.i) });
                    }
                    let k = self.string()?;
                    self.ws();
                    self.expect(b':')?;
                    let v = self.value(vm)?;
                    let _ = vm.create_data_property(o, PropKey::Str(k), v);
                    self.ws();
                    match self.peek() {
                        Some(x) if x == b',' as u16 => self.i += 1,
                        Some(x) if x == b'}' as u16 => {
                            self.i += 1;
                            return Ok(Value::Object(o));
                        }
                        Some(_) => return Err(PErr::Token(self.i)),
                        None => return Err(PErr::End),
                    }
                }
            }
            b'[' if c < 128 => {
                self.i += 1;
                let mut els = Vec::new();
                self.ws();
                if self.peek() == Some(b']' as u16) {
                    self.i += 1;
                    return Ok(Value::Object(vm.new_array(els)));
                }
                loop {
                    els.push(self.value(vm)?);
                    self.ws();
                    match self.peek() {
                        Some(x) if x == b',' as u16 => self.i += 1,
                        Some(x) if x == b']' as u16 => {
                            self.i += 1;
                            return Ok(Value::Object(vm.new_array(els)));
                        }
                        Some(_) => return Err(PErr::Token(self.i)),
                        None => return Err(PErr::End),
                    }
                }
            }
            b'"' if c < 128 => Ok(Value::String(self.string()?)),
            b't' if c < 128 => self.literal("true", Value::Bool(true)),
            b'f' if c < 128 => self.literal("false", Value::Bool(false)),
            b'n' if c < 128 => self.literal("null", Value::Null),
            b'-' | b'0'..=b'9' if c < 128 => self.number(),
            _ => Err(PErr::Token(self.i)),
        }
    }

    fn digits(&mut self) -> usize {
        let s = self.i;
        while self.peek().is_some_and(|c| (b'0' as u16..=b'9' as u16).contains(&c)) {
            self.i += 1;
        }
        self.i - s
    }

    fn number(&mut self) -> Result<Value, PErr> {
        let start = self.i;
        if self.peek() == Some(b'-' as u16) {
            self.i += 1;
        }
        match self.peek() {
            Some(c) if c == b'0' as u16 => self.i += 1,
            Some(c) if (b'1' as u16..=b'9' as u16).contains(&c) => {
                self.digits();
            }
            Some(_) => return Err(PErr::Token(self.i)),
            None => return Err(PErr::End),
        }
        if self.peek() == Some(b'.' as u16) {
            self.i += 1;
            if self.digits() == 0 {
                return Err(if self.peek().is_none() { PErr::End } else { PErr::Token(self.i) });
            }
        }
        if matches!(self.peek(), Some(c) if c == b'e' as u16 || c == b'E' as u16) {
            self.i += 1;
            if matches!(self.peek(), Some(c) if c == b'+' as u16 || c == b'-' as u16) {
                self.i += 1;
            }
            if self.digits() == 0 {
                return Err(if self.peek().is_none() { PErr::End } else { PErr::Token(self.i) });
            }
        }
        let text: String = self.s[start..self.i].iter().map(|&u| u as u8 as char).collect();
        Ok(Value::Number(text.parse().unwrap_or(f64::NAN)))
    }

    fn string(&mut self) -> Result<JsStr, PErr> {
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let Some(c) = self.peek() else { return Err(PErr::End) };
            self.i += 1;
            match c {
                0x22 => return Ok(JsStr::from_units(out)),
                0x5C => {
                    let Some(e) = self.peek() else { return Err(PErr::End) };
                    self.i += 1;
                    out.push(match e {
                        0x22 => 0x22,
                        0x5C => 0x5C,
                        0x2F => 0x2F,
                        0x62 => 0x08,
                        0x66 => 0x0C,
                        0x6E => 0x0A,
                        0x72 => 0x0D,
                        0x74 => 0x09,
                        0x75 => {
                            let mut v = 0u16;
                            for _ in 0..4 {
                                let Some(h) = self.peek() else { return Err(PErr::End) };
                                let d = char::from_u32(h as u32).and_then(|ch| ch.to_digit(16)).ok_or(PErr::Token(self.i))?;
                                v = v * 16 + d as u16;
                                self.i += 1;
                            }
                            v
                        }
                        _ => return Err(PErr::Token(self.i - 1)),
                    });
                }
                c if c < 0x20 => return Err(PErr::Token(self.i - 1)),
                c => out.push(c),
            }
        }
    }
}

fn internalize(vm: &mut Vm, reviver: &Value, holder: ObjRef, key: JsStr) -> R {
    let val = vm.get_with_receiver(holder, &PropKey::Str(key.clone()), &Value::Object(holder))?;
    let mark = vm.push_root(val.clone());
    let r = (|| {
        if let Value::Object(o) = val {
            let keys: Vec<JsStr> = if vm.heap.obj(o).is_array() {
                (0..super::array::len_of(vm, o)?).map(|i| JsStr::new(&i.to_string())).collect()
            } else {
                vm.own_enumerable_string_keys(o)
            };
            for k in keys {
                let nv = internalize(vm, reviver, o, k.clone())?;
                if nv.is_undefined() {
                    vm.delete_property(o, &PropKey::Str(k))?;
                } else {
                    vm.create_data_property(o, PropKey::Str(k), nv)?;
                }
            }
        }
        vm.call(reviver, Value::Object(holder), vec![Value::String(key), val.clone()])
    })();
    vm.pop_roots(mark);
    r
}

fn parse(vm: &mut Vm, c: &NativeCall) -> R {
    let text = vm.to_string(&c.arg(0))?;
    let mut p = Parser { s: text.units(), i: 0 };
    let r = p.value(vm).and_then(|v| {
        p.ws();
        if p.i < p.s.len() { Err(PErr::Token(p.i)) } else { Ok(v) }
    });
    let v = match r {
        Ok(v) => v,
        Err(PErr::End) => return Err(vm.syntax_error("Unexpected end of JSON input")),
        Err(PErr::Token(i)) => {
            let tok = String::from_utf16_lossy(&text.units()[i..i + 1]);
            let src = text.to_rust();
            let shown = if src.chars().count() <= 40 { format!("\"{src}\"") } else { format!("\"{}\"...", src.chars().take(40).collect::<String>()) };
            return Err(vm.syntax_error(format!("Unexpected token '{tok}', {shown} is not valid JSON")));
        }
    };
    let reviver = c.arg(1);
    if !vm.is_callable(&reviver) {
        return Ok(v);
    }
    let root = vm.new_object();
    vm.define_value(root, "", v, DEFAULT_FLAGS);
    let mark = vm.push_root(Value::Object(root));
    let r = internalize(vm, &reviver, root, JsStr::default());
    vm.pop_roots(mark);
    r
}
