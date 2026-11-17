//! Keel: the versioned C ABI shared by the Rust language layer and the Zig
//! machine layer.
//!
//! ABI v2 added doubles (`TAG_FLOAT`, bit-packed into the `i64` payload),
//! booleans, null/undefined, the arithmetic/comparison opcode set, the
//! `console.log` print intrinsic, and cross-language constant conformance
//! probes used to prove the hand-mirrored `zig/src/abi.zig` cannot drift.
//!
//! ABI v3 added locals (`OP_GET_LOCAL`/`OP_SET_LOCAL`), stack duplication,
//! and absolute-index jumps for control flow (`if`/`else`, `while`, `for`,
//! and short-circuit `&&`/`||`).
//!
//! ABI v4 adds strings: `TAG_STRING` (payload points at a `KeelString` in
//! machine-owned memory), the string-constant pool passed to a widened
//! `r8_machine_run` signature, `OP_LOAD_STRING` (pool index operand), and
//! `OP_TO_STRING` for template-literal substitution.
//!
//! ABI v5 adds functions: `TAG_FUNCTION` (payload points at a machine-owned
//! function object with captured upvalue cells), a function table (`start`,
//! `len`, `local_count`, `arity` per function, bodies inline in the flat
//! instruction stream), closure creation and capture opcodes, upvalue
//! access, and call frames inside the machine.
//!
//! ABI v6 adds arrays and objects: `TAG_ARRAY` / `TAG_OBJECT` (arena-owned
//! element/property storage), `TAG_METHOD` (a bound builtin: receiver +
//! method id), array/object construction, and unified property access
//! (`a.b` compiles to `a["b"]`) with JS GetV semantics.
//!
//! ABI v7 adds exceptions: `OP_TRY_ENTER` / `OP_TRY_EXIT` manage a handler
//! stack (catch pc, stack depth, frame), `OP_THROW` unwinds to the nearest
//! handler or out of the program, and `r8_machine_run` gains an `err_out`
//! parameter that receives the uncaught thrown value (or `undefined`).

pub const ABI_VERSION: u32 = 7;

/// Value tags. `CONFORMANCE_TAGS` lists these in a canonical order; keep both
/// in sync with `zig/src/abi.zig`.
pub const TAG_INT: u32 = 1;
pub const TAG_OBJECT: u32 = 2;
pub const TAG_FLOAT: u32 = 3;
pub const TAG_BOOL: u32 = 4;
pub const TAG_NULL: u32 = 5;
pub const TAG_UNDEFINED: u32 = 6;
pub const TAG_STRING: u32 = 7;
pub const TAG_FUNCTION: u32 = 8;
pub const TAG_ARRAY: u32 = 9;
pub const TAG_METHOD: u32 = 10;

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

/// Duplicates the top of the stack (for `&&`/`||`, whose conditional jumps
/// pop the condition but must leave the deciding value behind).
pub const OP_DUP: u32 = 25;
/// Pushes the value of local slot `operand`.
pub const OP_GET_LOCAL: u32 = 26;
/// Pops a value and stores it into local slot `operand`.
pub const OP_SET_LOCAL: u32 = 27;
/// Jumps to absolute instruction index `operand`.
pub const OP_JMP: u32 = 28;
/// Pops the condition; jumps to `operand` when it is falsy.
pub const OP_JMP_IF_FALSE: u32 = 29;
/// Pops the condition; jumps to `operand` when it is truthy.
pub const OP_JMP_IF_TRUE: u32 = 30;
/// Pushes a copy of string constant `operand` from the pool. The machine
/// copies the bytes into its own arena, so the pool only needs to outlive
/// the call.
pub const OP_LOAD_STRING: u32 = 31;
/// Pops a value and pushes its JS `String()` rendering as a string value
/// (used by template-literal substitutions).
pub const OP_TO_STRING: u32 = 32;
/// Pushes a function value for table entry `operand`; upvalue captures
/// (which follow immediately) append to its environment.
pub const OP_CLOSURE: u32 = 33;
/// Appends the cell of parent local slot `operand` to the function under
/// construction (the value on top of the stack).
pub const OP_CAPTURE_LOCAL: u32 = 34;
/// Appends the parent's upvalue cell `operand` to the function under
/// construction (transitive captures).
pub const OP_CAPTURE_UPVALUE: u32 = 35;
/// Pushes upvalue `operand` of the current function's environment.
pub const OP_GET_UPVALUE: u32 = 36;
/// Pops a value and stores it into upvalue `operand`'s cell.
pub const OP_SET_UPVALUE: u32 = 37;
/// Moves local slot `operand`'s value into a fresh heap cell (a captured
/// binding); subsequent `OP_GET_LOCAL`/`OP_SET_LOCAL` route through it.
pub const OP_MAKE_CELL: u32 = 38;
/// Calls the function value on the stack with the `operand` arguments above
/// it: pops both, pushes a call frame, and enters the function body. Missing
/// arguments read as `undefined`; extras are ignored. Calling a `TAG_METHOD`
/// value dispatches to the bound builtin instead.
pub const OP_CALL: u32 = 39;
/// Pops `operand` values and builds an array object in the machine's arena.
pub const OP_NEW_ARRAY: u32 = 40;
/// Pops `2 × operand` values (key, value pairs, key pushed first) and builds
/// an object in the machine's arena. Keys are string values.
pub const OP_NEW_OBJECT: u32 = 41;
/// Pops a key and a receiver; pushes the property per JS GetV semantics
/// (array elements and `.length`, builtin methods, object keys, string
/// indexing). Missing properties read as `undefined`.
pub const OP_GET_INDEX: u32 = 42;
/// Pops the key (top) and the value; the receiver stays (assignment
/// expressions need the receiver evaluated exactly once, and keeping it
/// lets plain and compound shapes align). Pushes the assigned value as the
/// expression's result.
pub const OP_SET_INDEX: u32 = 43;
/// Pushes an exception handler frame: on a throw within this frame, the
/// machine restores the try-entry stack depth and jumps to catch pc
/// `operand`, pushing the caught value.
pub const OP_TRY_ENTER: u32 = 44;
/// Pops the innermost exception handler (end of a try block).
pub const OP_TRY_EXIT: u32 = 45;
/// Pops the thrown value and unwinds: to the nearest handler in this frame
/// (restoring the try-entry state and pushing the caught value), or — after
/// unwinding call frames — out of the program through `err_out`.
pub const OP_THROW: u32 = 46;

/// Canonical tag order mirrored by `zig/src/abi.zig` for conformance probes.
/// New tags are appended at the end so existing indices stay stable.
pub const CONFORMANCE_TAGS: &[u32] = &[
    TAG_INT,
    TAG_OBJECT,
    TAG_FLOAT,
    TAG_BOOL,
    TAG_NULL,
    TAG_UNDEFINED,
    TAG_STRING,
    TAG_FUNCTION,
    TAG_ARRAY,
    TAG_METHOD,
];

/// Canonical opcode order mirrored by `zig/src/abi.zig` for conformance probes.
/// New opcodes are appended at the end so existing indices stay stable.
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
    OP_DUP,
    OP_GET_LOCAL,
    OP_SET_LOCAL,
    OP_JMP,
    OP_JMP_IF_FALSE,
    OP_JMP_IF_TRUE,
    OP_LOAD_STRING,
    OP_TO_STRING,
    OP_CLOSURE,
    OP_CAPTURE_LOCAL,
    OP_CAPTURE_UPVALUE,
    OP_GET_UPVALUE,
    OP_SET_UPVALUE,
    OP_MAKE_CELL,
    OP_CALL,
    OP_NEW_ARRAY,
    OP_NEW_OBJECT,
    OP_GET_INDEX,
    OP_SET_INDEX,
    OP_TRY_ENTER,
    OP_TRY_EXIT,
    OP_THROW,
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

    /// Resolves a string value's bytes. The returned slice points into
    /// machine-owned memory and is valid until the next `r8_machine_run`
    /// resets the machine's arena (pool constants live for the process
    /// lifetime).
    pub fn as_str(&self) -> Option<&'static str> {
        if self.tag != TAG_STRING {
            return None;
        }
        // SAFETY: the payload was produced by `string_literal` or by the
        // machine's string arena; both keep the `KeelString` and its bytes
        // alive as documented above.
        let keel_string = unsafe { &*(self.payload as *const KeelString) };
        let bytes = unsafe { std::slice::from_raw_parts(keel_string.ptr, keel_string.len) };
        std::str::from_utf8(bytes).ok()
    }

    /// Resolves an array value's element storage. The slice points into the
    /// machine's arena and is valid until the next `r8_machine_run`.
    pub fn as_array(&self) -> Option<&'static KeelArray> {
        (self.tag == TAG_ARRAY).then(|| unsafe { &*(self.payload as *const KeelArray) })
    }

    /// Resolves an object value's property storage, as with `as_array`.
    pub fn as_object(&self) -> Option<&'static KeelObject> {
        (self.tag == TAG_OBJECT).then(|| unsafe { &*(self.payload as *const KeelObject) })
    }

    /// Resolves a method value's binding, as with `as_array`.
    pub fn as_method(&self) -> Option<&'static KeelMethod> {
        (self.tag == TAG_METHOD).then(|| unsafe { &*(self.payload as *const KeelMethod) })
    }
}

/// A borrowed string in memory owned by whoever created the value (the Rust
/// pool for constants, the machine's arena for runtime values).
#[repr(C)]
pub struct KeelString {
    pub ptr: *const u8,
    pub len: usize,
}

/// One function's bytecode layout: a `[start, start+len)` range of the flat
/// instruction stream (self-terminating with `OP_RETURN`), its private local
/// slot count, and its parameter count. Entry 0 is the program's main body.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeelFunction {
    pub start: u32,
    pub len: u32,
    pub local_count: u32,
    pub arity: u32,
}

/// An array's element storage in the machine's arena.
#[repr(C)]
pub struct KeelArray {
    pub items: *mut Value,
    pub len: usize,
}

/// An object's property storage: parallel key/value arrays of values (keys
/// are `TAG_STRING` values), scanned linearly.
#[repr(C)]
pub struct KeelObject {
    pub keys: *mut Value,
    pub values: *mut Value,
    pub len: usize,
}

/// A bound builtin method: which builtin, and the receiver it was read
/// from. Built-in method ids are defined by the machine.
#[repr(C)]
pub struct KeelMethod {
    pub receiver: Value,
    pub id: u32,
}

impl Value {
    /// Builds a pool constant for `bytes`. The bytes are copied and leaked:
    /// pool constants intentionally live for the process lifetime, which is
    /// fine for compile-per-run embedders and revisited with the GC
    /// (Phase 6).
    pub fn string_literal(bytes: &str) -> Self {
        let data = Box::leak(bytes.as_bytes().to_vec().into_boxed_slice());
        let keel_string = Box::leak(Box::new(KeelString {
            ptr: data.as_ptr(),
            len: data.len(),
        }));
        Self {
            tag: TAG_STRING,
            _pad: 0,
            payload: keel_string as *const KeelString as i64,
        }
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
            TAG_STRING => match self.as_str() {
                Some(text) => f.write_str(text),
                None => f.write_str("<string>"),
            },
            TAG_FUNCTION => f.write_str("[function]"),
            TAG_ARRAY => {
                f.write_str("")?;
                // JS `String(array)` joins elements with "," (null and
                // undefined render as empty).
                if let Some(array) = self.as_array() {
                    let items = unsafe { std::slice::from_raw_parts(array.items, array.len) };
                    for (index, item) in items.iter().enumerate() {
                        if index > 0 {
                            f.write_str(",")?;
                        }
                        match item.tag {
                            TAG_NULL | TAG_UNDEFINED => {}
                            _ => write!(f, "{item}")?,
                        }
                    }
                }
                Ok(())
            }
            TAG_OBJECT => f.write_str("[object Object]"),
            TAG_METHOD => f.write_str("[method]"),
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

    pub const fn load_string(index: usize) -> Self {
        Self {
            op: OP_LOAD_STRING,
            operand: index as i32,
        }
    }

    pub const fn closure(index: usize) -> Self {
        Self {
            op: OP_CLOSURE,
            operand: index as i32,
        }
    }

    pub const fn capture_local(slot: usize) -> Self {
        Self {
            op: OP_CAPTURE_LOCAL,
            operand: slot as i32,
        }
    }

    pub const fn capture_upvalue(index: usize) -> Self {
        Self {
            op: OP_CAPTURE_UPVALUE,
            operand: index as i32,
        }
    }

    pub const fn get_upvalue(index: usize) -> Self {
        Self {
            op: OP_GET_UPVALUE,
            operand: index as i32,
        }
    }

    pub const fn set_upvalue(index: usize) -> Self {
        Self {
            op: OP_SET_UPVALUE,
            operand: index as i32,
        }
    }

    pub const fn make_cell(slot: usize) -> Self {
        Self {
            op: OP_MAKE_CELL,
            operand: slot as i32,
        }
    }

    pub const fn call(arg_count: usize) -> Self {
        Self {
            op: OP_CALL,
            operand: arg_count as i32,
        }
    }

    pub const fn new_array(element_count: usize) -> Self {
        Self {
            op: OP_NEW_ARRAY,
            operand: element_count as i32,
        }
    }

    pub const fn new_object(property_count: usize) -> Self {
        Self {
            op: OP_NEW_OBJECT,
            operand: property_count as i32,
        }
    }

    pub const fn get_index() -> Self {
        Self {
            op: OP_GET_INDEX,
            operand: 0,
        }
    }

    pub const fn set_index() -> Self {
        Self {
            op: OP_SET_INDEX,
            operand: 0,
        }
    }

    pub const fn try_enter(catch_pc: usize) -> Self {
        Self {
            op: OP_TRY_ENTER,
            operand: catch_pc as i32,
        }
    }

    pub const fn try_exit() -> Self {
        Self {
            op: OP_TRY_EXIT,
            operand: 0,
        }
    }

    pub const fn throw() -> Self {
        Self {
            op: OP_THROW,
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

    pub const fn dup() -> Self {
        Self {
            op: OP_DUP,
            operand: 0,
        }
    }

    pub const fn get_local(slot: usize) -> Self {
        Self {
            op: OP_GET_LOCAL,
            operand: slot as i32,
        }
    }

    pub const fn set_local(slot: usize) -> Self {
        Self {
            op: OP_SET_LOCAL,
            operand: slot as i32,
        }
    }

    /// Jumps carry absolute instruction indices; the compiler patches the
    /// operand after emitting the target.
    pub const fn jmp(target: usize) -> Self {
        Self {
            op: OP_JMP,
            operand: target as i32,
        }
    }

    pub const fn jmp_if_false(target: usize) -> Self {
        Self {
            op: OP_JMP_IF_FALSE,
            operand: target as i32,
        }
    }

    pub const fn jmp_if_true(target: usize) -> Self {
        Self {
            op: OP_JMP_IF_TRUE,
            operand: target as i32,
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
    /// Runs bytecode with a constant pool and a function table. Pool entries
    /// referenced by `OP_LOAD_STRING` (tag `TAG_STRING`) are copied into the
    /// machine's arena when loaded, so the pool only needs to outlive the
    /// call. The function table likewise only needs to outlive the call.
    /// Returned string and function values point into the machine's arena
    /// and are valid until the next call resets it.
    pub fn r8_machine_run(
        instructions: *const Instruction,
        len: usize,
        pool: *const Value,
        pool_len: usize,
        functions: *const KeelFunction,
        functions_len: usize,
        // Receives the uncaught thrown value on failure, or `undefined` on
        // success. Thrown values point into the machine's arena (valid
        // until the next run).
        err_out: *mut Value,
    ) -> Value;

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
    fn string_literal_round_trip() {
        let value = Value::string_literal("hello");
        assert_eq!(value.as_str(), Some("hello"));
        assert_eq!(value.as_int(), None);
        assert_eq!(value.to_string(), "hello");
        assert_eq!(Value::string_literal("héllo ✓").as_str(), Some("héllo ✓"));
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

    #[test]
    fn conformance_tables_cover_every_constant() {
        // Every TAG_* and OP_* constant must appear in its conformance table
        // (the anti-drift gate is only as good as its coverage).
        let tags = [
            TAG_INT,
            TAG_OBJECT,
            TAG_FLOAT,
            TAG_BOOL,
            TAG_NULL,
            TAG_UNDEFINED,
            TAG_STRING,
            TAG_FUNCTION,
            TAG_ARRAY,
            TAG_METHOD,
        ];
        for tag in tags {
            assert!(
                CONFORMANCE_TAGS.contains(&tag),
                "tag {tag} missing from CONFORMANCE_TAGS"
            );
        }
        for op in CONFORMANCE_OPS {
            let _ = op;
        }
        let ops = [
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
            OP_DUP,
            OP_GET_LOCAL,
            OP_SET_LOCAL,
            OP_JMP,
            OP_JMP_IF_FALSE,
            OP_JMP_IF_TRUE,
            OP_LOAD_STRING,
            OP_TO_STRING,
            OP_CLOSURE,
            OP_CAPTURE_LOCAL,
            OP_CAPTURE_UPVALUE,
            OP_GET_UPVALUE,
            OP_SET_UPVALUE,
            OP_MAKE_CELL,
            OP_CALL,
            OP_NEW_ARRAY,
            OP_NEW_OBJECT,
            OP_GET_INDEX,
            OP_SET_INDEX,
            OP_TRY_ENTER,
            OP_TRY_EXIT,
            OP_THROW,
        ];
        for op in ops {
            assert!(
                CONFORMANCE_OPS.contains(&op),
                "opcode {op} missing from CONFORMANCE_OPS"
            );
        }
        assert_eq!(CONFORMANCE_TAGS.len(), tags.len());
        assert_eq!(CONFORMANCE_OPS.len(), ops.len());
    }
}
