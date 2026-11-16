//! Keel: the versioned C ABI shared by the Rust language layer and the Zig
//! machine layer.
//!
//! ABI v2 adds doubles (`TAG_FLOAT`, bit-packed into the `i64` payload),
//! booleans, null/undefined, the arithmetic/comparison opcode set, the
//! `console.log` print intrinsic, and cross-language constant conformance
//! probes used to prove the hand-mirrored `zig/src/abi.zig` cannot drift.

pub const ABI_VERSION: u32 = 2;

/// Value tags. `CONFORMANCE_TAGS` lists these in a canonical order; keep both
/// in sync with `zig/src/abi.zig`.
pub const TAG_INT: u32 = 1;
pub const TAG_OBJECT: u32 = 2;
pub const TAG_FLOAT: u32 = 3;
pub const TAG_BOOL: u32 = 4;
pub const TAG_NULL: u32 = 5;
pub const TAG_UNDEFINED: u32 = 6;

/// Opcodes. `CONFORMANCE_OPS` lists these in a canonical order; keep both in
/// sync with `zig/src/abi.zig`.
pub const OP_LOAD_INT: u32 = 0;
pub const OP_ADD: u32 = 1;
pub const OP_RETURN: u32 = 2;
/// Pushes the (sign-extended) high half of a float literal as raw data.
pub const OP_LOAD_F64_HI: u32 = 3;
/// Pops the high half pushed by `OP_LOAD_F64_HI`; `operand` carries the low
/// half. Together the two instructions encode a 64-bit float literal within
/// the 8-byte instruction format.
pub const OP_LOAD_F64: u32 = 4;
pub const OP_SUB: u32 = 5;
pub const OP_MUL: u32 = 6;
pub const OP_DIV: u32 = 7;
pub const OP_MOD: u32 = 8;
pub const OP_POW: u32 = 9;
pub const OP_LT: u32 = 10;
pub const OP_GT: u32 = 11;
pub const OP_LE: u32 = 12;
pub const OP_GE: u32 = 13;
pub const OP_EQ: u32 = 14;
pub const OP_NE: u32 = 15;
pub const OP_EQ_STRICT: u32 = 16;
pub const OP_NE_STRICT: u32 = 17;
pub const OP_NEG: u32 = 18;
pub const OP_NOT: u32 = 19;
pub const OP_LOAD_BOOL: u32 = 20;
pub const OP_LOAD_NULL: u32 = 21;
pub const OP_LOAD_UNDEFINED: u32 = 22;
/// `console.log` intrinsic: pops a value, prints it as JS does, pushes
/// `undefined` so every expression still leaves exactly one value. Replaced
/// by a host-function mechanism once calls exist.
pub const OP_PRINT: u32 = 23;
pub const OP_POP: u32 = 24;

/// Canonical tag order mirrored by `zig/src/abi.zig` for conformance probes.
pub const CONFORMANCE_TAGS: &[u32] = &[
    TAG_INT,
    TAG_OBJECT,
    TAG_FLOAT,
    TAG_BOOL,
    TAG_NULL,
    TAG_UNDEFINED,
];

/// Canonical opcode order mirrored by `zig/src/abi.zig` for conformance probes.
pub const CONFORMANCE_OPS: &[u32] = &[
    OP_LOAD_INT,
    OP_ADD,
    OP_RETURN,
    OP_LOAD_F64_HI,
    OP_LOAD_F64,
    OP_SUB,
    OP_MUL,
    OP_DIV,
    OP_MOD,
    OP_POW,
    OP_LT,
    OP_GT,
    OP_LE,
    OP_GE,
    OP_EQ,
    OP_NE,
    OP_EQ_STRICT,
    OP_NE_STRICT,
    OP_NEG,
    OP_NOT,
    OP_LOAD_BOOL,
    OP_LOAD_NULL,
    OP_LOAD_UNDEFINED,
    OP_PRINT,
    OP_POP,
];

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Value {
    pub tag: u32,
    pub _pad: u32,
    pub payload: i64,
}

impl Value {
    pub const NULL: Self = Self {
        tag: TAG_NULL,
        _pad: 0,
        payload: 0,
    };

    pub const UNDEFINED: Self = Self {
        tag: TAG_UNDEFINED,
        _pad: 0,
        payload: 0,
    };

    pub const fn int(value: i64) -> Self {
        Self {
            tag: TAG_INT,
            _pad: 0,
            payload: value,
        }
    }

    pub const fn float(value: f64) -> Self {
        Self {
            tag: TAG_FLOAT,
            _pad: 0,
            payload: value.to_bits() as i64,
        }
    }

    pub const fn boolean(value: bool) -> Self {
        Self {
            tag: TAG_BOOL,
            _pad: 0,
            payload: value as i64,
        }
    }

    pub fn as_int(self) -> Option<i64> {
        (self.tag == TAG_INT).then_some(self.payload)
    }

    pub fn as_float(self) -> Option<f64> {
        (self.tag == TAG_FLOAT).then(|| f64::from_bits(self.payload as u64))
    }

    pub fn as_bool(self) -> Option<bool> {
        (self.tag == TAG_BOOL).then_some(self.payload != 0)
    }
}

// SAFETY notes: `Value` crosses the FFI boundary by value in `r8_machine_run`
// and in the conformance probes; the layout is asserted below and mirrored in
// `zig/src/abi.zig`.
impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.tag {
            TAG_INT => write!(f, "{}", self.payload),
            TAG_FLOAT => js_number(f64::from_bits(self.payload as u64), f),
            TAG_BOOL => f.write_str(if self.payload != 0 { "true" } else { "false" }),
            TAG_NULL => f.write_str("null"),
            TAG_UNDEFINED => f.write_str("undefined"),
            // Placeholder until objects exist (Phase 5).
            TAG_OBJECT => f.write_str("[object]"),
            _ => write!(f, "<value tag={}>", self.tag),
        }
    }
}

/// Formats a double exactly like JavaScript's `Number::toString`:
/// shortest round-trip digits, fixed notation for exponents in (-6, 21),
/// scientific notation otherwise (`1e+21`, `1e-7`), `NaN`/`Infinity`,
/// and `"0"` for both signed zeros.
fn js_number(x: f64, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    if x.is_nan() {
        return f.write_str("NaN");
    }
    if x == 0.0 {
        return f.write_str("0");
    }
    if x < 0.0 {
        f.write_str("-")?;
        return js_number(-x, f);
    }
    if x.is_infinite() {
        return f.write_str("Infinity");
    }
    // `{:e}` yields the shortest round-trip representation in scientific
    // form, e.g. `3.0000000000000004e-1`. Split it into digits and exponent.
    let scientific = format!("{:e}", x);
    let (mantissa, exponent) = scientific.split_once('e').expect("LowerExp emits 'e'");
    let exponent: i32 = exponent.parse().expect("exponent is a decimal integer");
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    js_number_digits(&digits, exponent + 1, f)
}

/// Emits digits `d1..dk` where the value is `d1.d2..dk × 10^(n-1)`, following
/// the notation rules of ECMA-262 `Number::toString`.
fn js_number_digits(digits: &str, n: i32, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    let k = digits.len() as i32;
    if k <= n && n <= 21 {
        f.write_str(digits)?;
        for _ in 0..(n - k) {
            f.write_str("0")?;
        }
    } else if 0 < n && n <= 21 {
        f.write_str(&digits[..n as usize])?;
        f.write_str(".")?;
        f.write_str(&digits[n as usize..])?;
    } else if -6 < n && n <= 0 {
        f.write_str("0.")?;
        for _ in 0..(-n) {
            f.write_str("0")?;
        }
        f.write_str(digits)?;
    } else {
        f.write_str(&digits[..1])?;
        if k > 1 {
            f.write_str(".")?;
            f.write_str(&digits[1..])?;
        }
        let exponent = n - 1;
        write!(
            f,
            "e{}{}",
            if exponent < 0 { '-' } else { '+' },
            exponent.abs()
        )?;
    }
    Ok(())
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Instruction {
    pub op: u32,
    pub operand: i32,
}

impl Instruction {
    pub const fn load_int(value: i64) -> Self {
        Self {
            op: OP_LOAD_INT,
            operand: value as i32,
        }
    }

    /// Emits the two-instruction encoding of a 64-bit float literal.
    pub const fn load_f64(value: f64) -> [Instruction; 2] {
        let bits = value.to_bits();
        [
            Self {
                op: OP_LOAD_F64_HI,
                operand: (bits >> 32) as u32 as i32,
            },
            Self {
                op: OP_LOAD_F64,
                operand: bits as u32 as i32,
            },
        ]
    }

    pub const fn binary(op: u32) -> Self {
        Self { op, operand: 0 }
    }

    pub const fn unary(op: u32) -> Self {
        Self { op, operand: 0 }
    }

    pub const fn load_bool(value: bool) -> Self {
        Self {
            op: OP_LOAD_BOOL,
            operand: value as i32,
        }
    }

    pub const fn load_null() -> Self {
        Self {
            op: OP_LOAD_NULL,
            operand: 0,
        }
    }

    pub const fn load_undefined() -> Self {
        Self {
            op: OP_LOAD_UNDEFINED,
            operand: 0,
        }
    }

    pub const fn print() -> Self {
        Self {
            op: OP_PRINT,
            operand: 0,
        }
    }

    pub const fn pop() -> Self {
        Self {
            op: OP_POP,
            operand: 0,
        }
    }

    pub const fn add() -> Self {
        Self::binary(OP_ADD)
    }

    pub const fn return_() -> Self {
        Self {
            op: OP_RETURN,
            operand: 0,
        }
    }
}

unsafe extern "C" {
    pub fn r8_abi_version() -> u32;
    pub fn r8_machine_run(instructions: *const Instruction, len: usize) -> Value;

    /// Conformance probes exported by `zig/src/abi.zig`: the Zig machine
    /// reports its tag/opcode tables so the Rust side can prove both halves
    /// agree on every constant. Table order must match the `CONFORMANCE_*`
    /// lists above.
    pub fn r8_abi_tag_count() -> usize;
    pub fn r8_abi_tag_at(index: usize) -> u32;
    pub fn r8_abi_op_count() -> usize;
    pub fn r8_abi_op_at(index: usize) -> u32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_stable() {
        assert_eq!(std::mem::size_of::<Value>(), 16);
        assert_eq!(std::mem::align_of::<Value>(), 8);
        assert_eq!(std::mem::size_of::<Instruction>(), 8);
        assert_eq!(std::mem::align_of::<Instruction>(), 4);
    }

    #[test]
    fn int_round_trip() {
        assert_eq!(Value::int(42).as_int(), Some(42));
        assert_eq!(Value::int(-7).as_int(), Some(-7));
    }

    #[test]
    fn float_round_trip() {
        assert_eq!(Value::float(0.5).as_float(), Some(0.5));
        assert_eq!(Value::float(-2.75).as_float(), Some(-2.75));
        assert_eq!(Value::float(f64::INFINITY).as_float(), Some(f64::INFINITY));
    }

    #[test]
    fn float_literal_encoding_round_trips() {
        let instructions = Instruction::load_f64(0.30000000000000004);
        assert_eq!(instructions[0].op, OP_LOAD_F64_HI);
        assert_eq!(instructions[1].op, OP_LOAD_F64);
        let high = instructions[0].operand as i64;
        let low = instructions[1].operand as i64;
        let bits = ((high as u64) << 32) | (low as u64 & 0xFFFF_FFFF);
        assert_eq!(f64::from_bits(bits), 0.30000000000000004);
    }

    #[test]
    fn display_matches_js_number_to_string() {
        let cases: &[(f64, &str)] = &[
            (0.0, "0"),
            (-0.0, "0"),
            (1.0, "1"),
            (-7.0, "-7"),
            (100.0, "100"),
            (0.5, "0.5"),
            (0.30000000000000004, "0.30000000000000004"),
            (1.0 / 3.0, "0.3333333333333333"),
            (2.0 / 3.0, "0.6666666666666666"),
            (123456789.25, "123456789.25"),
            (0.0001, "0.0001"),
            (1e-6, "0.000001"),
            (1e-7, "1e-7"),
            (1e21, "1e+21"),
            (1e23, "1e+23"),
            (1e20, "100000000000000000000"),
            (9007199254740992.0, "9007199254740992"),
            (1.8446744073709552e19, "18446744073709552000"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
            (f64::NAN, "NaN"),
        ];
        for (value, expected) in cases {
            assert_eq!(Value::float(*value).to_string(), *expected, "for {value:?}");
        }
    }

    #[test]
    fn display_matches_js_for_other_tags() {
        assert_eq!(Value::int(42).to_string(), "42");
        assert_eq!(Value::boolean(true).to_string(), "true");
        assert_eq!(Value::boolean(false).to_string(), "false");
        assert_eq!(Value::NULL.to_string(), "null");
        assert_eq!(Value::UNDEFINED.to_string(), "undefined");
    }
}
