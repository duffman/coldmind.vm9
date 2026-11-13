//! Constant folding over primitive constants and branch folding.
//!
//! Only operations whose result is fully determined by primitive operands
//! and that cannot throw or call user code are folded. Number-to-string
//! conversion is deliberately not folded (the VM owns that algorithm).

use super::Pass;
use crate::ir::*;

pub struct ConstFold;

impl Pass for ConstFold {
    fn name(&self) -> &'static str {
        "const-fold"
    }

    fn run(&mut self, f: &mut Function) -> bool {
        let mut consts: Vec<Option<Constant>> = vec![None; f.temp_count as usize];
        let mut changed = false;
        // Temps are single-assignment and defs dominate uses; one sweep in
        // block order suffices for chains inside a block, and the fixed-point
        // driver in `optimize` handles cross-block chains.
        for b in &mut f.blocks {
            for inst in &mut b.insts {
                let folded = match &inst.kind {
                    InstKind::Const { dst, value } => {
                        consts[dst.index()] = Some(value.clone());
                        None
                    }
                    InstKind::Unary { dst, op, src } => {
                        consts[src.index()].as_ref().and_then(|c| fold_unary(*op, c)).map(|v| (*dst, v))
                    }
                    InstKind::Binary { dst, op, lhs, rhs } => match (&consts[lhs.index()], &consts[rhs.index()]) {
                        (Some(a), Some(b)) => fold_binary(*op, a, b).map(|v| (*dst, v)),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some((dst, value)) = folded {
                    consts[dst.index()] = Some(value.clone());
                    inst.kind = InstKind::Const { dst, value };
                    changed = true;
                }
            }
            if let TermKind::Branch { cond, then_bb, else_bb } = b.term.kind {
                if let Some(c) = &consts[cond.index()] {
                    b.term.kind = TermKind::Jump(if c.truthy() { then_bb } else { else_bb });
                    changed = true;
                }
            }
        }
        changed
    }
}

pub fn fold_unary(op: UnaryOp, c: &Constant) -> Option<Constant> {
    use Constant::*;
    Some(match (op, c) {
        (UnaryOp::Not, c) => Bool(!c.truthy()),
        (UnaryOp::TypeOf, c) => String(c.type_of().into()),
        (UnaryOp::Neg, Number(n)) => Number(-n),
        (UnaryOp::Plus, Number(n)) => Number(*n),
        (UnaryOp::Plus, Bool(b)) => Number(*b as u8 as f64),
        (UnaryOp::Plus, Null) => Number(0.0),
        (UnaryOp::Plus, Undefined) => Number(f64::NAN),
        (UnaryOp::BitNot, Number(n)) => Number(!to_int32(*n) as f64),
        (UnaryOp::Inc, Number(n)) => Number(n + 1.0),
        (UnaryOp::Dec, Number(n)) => Number(n - 1.0),
        (UnaryOp::ToString, String(s)) | (UnaryOp::ToPropertyKey, String(s)) => String(s.clone()),
        (UnaryOp::ToString, Bool(b)) => String(if *b { "true" } else { "false" }.into()),
        (UnaryOp::ToString, Null) => String("null".into()),
        (UnaryOp::ToString, Undefined) => String("undefined".into()),
        _ => return None,
    })
}

pub fn fold_binary(op: BinaryOp, a: &Constant, b: &Constant) -> Option<Constant> {
    use Constant::*;
    if let (Number(x), Number(y)) = (a, b) {
        let (x, y) = (*x, *y);
        return Some(match op {
            BinaryOp::Add => Number(x + y),
            BinaryOp::Sub => Number(x - y),
            BinaryOp::Mul => Number(x * y),
            BinaryOp::Div => Number(x / y),
            BinaryOp::Mod => Number(x % y),
            BinaryOp::Exp => Number(js_pow(x, y)),
            BinaryOp::Shl => Number(to_int32(x).wrapping_shl(to_uint32(y) & 31) as f64),
            BinaryOp::Shr => Number((to_int32(x) >> (to_uint32(y) & 31)) as f64),
            BinaryOp::UShr => Number((to_uint32(x) >> (to_uint32(y) & 31)) as f64),
            BinaryOp::BitAnd => Number((to_int32(x) & to_int32(y)) as f64),
            BinaryOp::BitOr => Number((to_int32(x) | to_int32(y)) as f64),
            BinaryOp::BitXor => Number((to_int32(x) ^ to_int32(y)) as f64),
            BinaryOp::Eq | BinaryOp::StrictEq => Bool(x == y),
            BinaryOp::Ne | BinaryOp::StrictNe => Bool(x != y),
            BinaryOp::Lt => Bool(x < y),
            BinaryOp::Le => Bool(x <= y),
            BinaryOp::Gt => Bool(x > y),
            BinaryOp::Ge => Bool(x >= y),
            BinaryOp::InstanceOf | BinaryOp::In => return None,
        });
    }
    if let (String(x), String(y)) = (a, b) {
        let ord = x.encode_utf16().cmp(y.encode_utf16());
        return Some(match op {
            BinaryOp::Add => String(format!("{x}{y}").into()),
            BinaryOp::Eq | BinaryOp::StrictEq => Bool(x == y),
            BinaryOp::Ne | BinaryOp::StrictNe => Bool(x != y),
            BinaryOp::Lt => Bool(ord.is_lt()),
            BinaryOp::Le => Bool(ord.is_le()),
            BinaryOp::Gt => Bool(ord.is_gt()),
            BinaryOp::Ge => Bool(ord.is_ge()),
            _ => return None,
        });
    }
    match op {
        BinaryOp::StrictEq => Some(Bool(strict_eq(a, b))),
        BinaryOp::StrictNe => Some(Bool(!strict_eq(a, b))),
        BinaryOp::Eq | BinaryOp::Ne => {
            let nullish = |c: &Constant| matches!(c, Null | Undefined);
            let eq = if nullish(a) || nullish(b) {
                nullish(a) && nullish(b)
            } else if std::mem::discriminant(a) == std::mem::discriminant(b) {
                strict_eq(a, b)
            } else {
                return None;
            };
            Some(Bool(if op == BinaryOp::Eq { eq } else { !eq }))
        }
        _ => None,
    }
}

fn strict_eq(a: &Constant, b: &Constant) -> bool {
    use Constant::*;
    match (a, b) {
        (Undefined, Undefined) | (Null, Null) => true,
        (Bool(x), Bool(y)) => x == y,
        (Number(x), Number(y)) => x == y,
        (String(x), String(y)) => x == y,
        _ => false,
    }
}

pub fn to_int32(n: f64) -> i32 {
    to_uint32(n) as i32
}

pub fn to_uint32(n: f64) -> u32 {
    if !n.is_finite() {
        return 0;
    }
    let t = n.trunc();
    let m = t.rem_euclid(4294967296.0);
    m as u32
}

/// `Math.pow` / `**` with ECMAScript edge cases.
pub fn js_pow(x: f64, y: f64) -> f64 {
    if y.is_nan() {
        return f64::NAN;
    }
    if y == 0.0 {
        return 1.0;
    }
    if (x == 1.0 || x == -1.0) && y.is_infinite() {
        return f64::NAN;
    }
    x.powf(y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_numbers_and_strings() {
        use Constant::*;
        assert_eq!(fold_binary(BinaryOp::Add, &Number(1.0), &Number(2.0)), Some(Number(3.0)));
        assert_eq!(fold_binary(BinaryOp::Add, &String("a".into()), &String("b".into())), Some(String("ab".into())));
        // Mixed string + number needs Number::toString: not folded.
        assert_eq!(fold_binary(BinaryOp::Add, &String("a".into()), &Number(1.0)), None);
        assert_eq!(fold_binary(BinaryOp::Eq, &Null, &Undefined), Some(Bool(true)));
        assert_eq!(fold_binary(BinaryOp::StrictEq, &Number(f64::NAN), &Number(f64::NAN)), Some(Bool(false)));
        assert_eq!(fold_binary(BinaryOp::Shl, &Number(1.0), &Number(33.0)), Some(Number(2.0)));
        assert_eq!(fold_binary(BinaryOp::UShr, &Number(-1.0), &Number(0.0)), Some(Number(4294967295.0)));
        assert!(matches!(fold_binary(BinaryOp::Exp, &Number(1.0), &Number(f64::INFINITY)), Some(Number(n)) if n.is_nan()));
    }
}
