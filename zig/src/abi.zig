//! ABI version note: this file hand-mirrors `crates/r8-abi`. ABI v2 added
//! doubles/booleans/print; ABI v3 added locals, DUP, and jumps; ABI v4 adds
//! strings (TAG_STRING, constant pool, LOAD_STRING/TO_STRING); ABI v5 adds
//! functions (TAG_FUNCTION, function table, closure/capture opcodes); ABI v6
//! adds arrays and objects (TAG_ARRAY, TAG_OBJECT, TAG_METHOD, property
//! access); ABI v7 adds exceptions (TRY_ENTER/TRY_EXIT/THROW, err_out);
//! ABI v9 adds prototype chains (KeelObject.proto, OP_INSTANCEOF).

const std = @import("std");

pub const ABI_VERSION: u32 = 9;

/// Value tags. Mirrors `crates/r8-abi`'s constants; the `CONFORMANCE_TAGS`
/// table below lists them in the same canonical order — keep both in sync.
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

/// Opcodes. Mirrors `crates/r8-abi`; the `CONFORMANCE_OPS` table below lists
/// them in the same canonical order — keep both in sync.
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
/// Pushes `this`: the run's global object (sloppy-mode global semantics).
pub const OP_THIS: u32 = 47;
/// Pops a value; pushes its `typeof` string ("undefined", "boolean",
/// "number", "string", "function", or "object" — including for null).
pub const OP_TYPEOF: u32 = 48;
/// 32-bit bitwise AND (`ToInt32` on both operands).
pub const OP_BIT_AND: u32 = 49;
/// 32-bit bitwise OR.
pub const OP_BIT_OR: u32 = 50;
/// 32-bit bitwise XOR.
pub const OP_BIT_XOR: u32 = 51;
/// 32-bit left shift (`ToInt32(a) << (ToInt32(b) & 31)`).
pub const OP_SHL: u32 = 52;
/// 32-bit signed right shift (arithmetic).
pub const OP_SHR: u32 = 53;
/// 32-bit unsigned right shift (`u32`, result is the unsigned value).
pub const OP_USHR: u32 = 54;
/// 32-bit bitwise NOT.
pub const OP_BIT_NOT: u32 = 55;
/// Pops a key and a receiver; pushes whether the key is a property of the
/// receiver (`key in receiver`). Non-object receivers throw TypeError.
pub const OP_IN: u32 = 56;
/// Pops a key and a receiver; deletes the property and pushes `true`
/// (configurability is not modeled yet). Non-object receivers throw
/// TypeError.
pub const OP_DELETE: u32 = 57;
/// Pops a receiver; pushes its keys as an array of values (object keys,
/// array indices as strings) for `for-in`.
pub const OP_KEYS: u32 = 58;
/// Constructor call: pops `operand` args and the callee; creates a fresh
/// object, binds it as the callee's `this`, enters the body, and — following
/// JS constructor semantics — uses the fresh object as the result unless the
/// callee returns an object of its own.
pub const OP_NEW: u32 = 60;
/// Pushes global `operand` (a string-pool name): the global property value,
/// or a ReferenceError throw when absent.
pub const OP_GLOBAL_GET: u32 = 61;
/// Pops a value; assigns it to global `operand` (a string-pool name) and
/// pushes it back (sloppy-mode global creation).
pub const OP_GLOBAL_SET: u32 = 62;
/// Pops a constructor function and a value; pushes whether the value's
/// prototype chain contains the constructor's `prototype` object.
pub const OP_INSTANCEOF: u32 = 63;

/// Canonical tag order mirrored from `crates/r8-abi`'s `CONFORMANCE_TAGS`.
const CONFORMANCE_TAGS = [_]u32{ TAG_INT, TAG_OBJECT, TAG_FLOAT, TAG_BOOL, TAG_NULL, TAG_UNDEFINED, TAG_STRING, TAG_FUNCTION, TAG_ARRAY, TAG_METHOD };

/// Canonical opcode order mirrored from `crates/r8-abi`'s `CONFORMANCE_OPS`.
/// New opcodes are appended at the end so existing indices stay stable.
const CONFORMANCE_OPS = [_]u32{
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
    OP_THIS,
    OP_TYPEOF,
    OP_BIT_AND,
    OP_BIT_OR,
    OP_BIT_XOR,
    OP_SHL,
    OP_SHR,
    OP_USHR,
    OP_BIT_NOT,
    OP_IN,
    OP_DELETE,
    OP_KEYS,
    OP_NEW,
    OP_GLOBAL_GET,
    OP_GLOBAL_SET,
    OP_INSTANCEOF,
};

pub const Value = extern struct {
    tag: u32,
    _pad: u32,
    payload: i64,

    pub const NULL: Value = .{ .tag = TAG_NULL, ._pad = 0, .payload = 0 };
    pub const UNDEF: Value = .{ .tag = TAG_UNDEFINED, ._pad = 0, .payload = 0 };

    pub fn fromInt(value: i64) Value {
        return .{ .tag = TAG_INT, ._pad = 0, .payload = value };
    }

    pub fn fromFloat(value: f64) Value {
        return .{ .tag = TAG_FLOAT, ._pad = 0, .payload = @bitCast(value) };
    }

    pub fn fromBool(value: bool) Value {
        return .{ .tag = TAG_BOOL, ._pad = 0, .payload = @intFromBool(value) };
    }

    pub fn asInt(self: Value) ?i64 {
        if (self.tag != TAG_INT) return null;
        return self.payload;
    }

    pub fn asFloat(self: Value) ?f64 {
        if (self.tag != TAG_FLOAT) return null;
        return @bitCast(@as(u64, @bitCast(self.payload)));
    }

    pub fn asBool(self: Value) ?bool {
        if (self.tag != TAG_BOOL) return null;
        return self.payload != 0;
    }

    /// Resolves a string value's bytes. The slice points into memory owned
    /// by the value's creator (the Rust pool or the machine's arena) and is
    /// valid as documented in `crates/r8-abi`.
    pub fn asString(self: Value) ?[]const u8 {
        if (self.tag != TAG_STRING) return null;
        const keel_string: *const KeelString = @ptrFromInt(@as(usize, @bitCast(self.payload)));
        return keel_string.ptr[0..keel_string.len];
    }

    pub fn asArray(self: Value) ?*KeelArray {
        if (self.tag != TAG_ARRAY) return null;
        return @ptrFromInt(@as(usize, @bitCast(self.payload)));
    }

    pub fn asObject(self: Value) ?*KeelObject {
        if (self.tag != TAG_OBJECT) return null;
        return @ptrFromInt(@as(usize, @bitCast(self.payload)));
    }

    pub fn asMethod(self: Value) ?*KeelMethod {
        if (self.tag != TAG_METHOD) return null;
        return @ptrFromInt(@as(usize, @bitCast(self.payload)));
    }

    pub fn isNumber(self: Value) bool {
        return self.tag == TAG_INT or self.tag == TAG_FLOAT;
    }

    /// JS `ToNumber` over the value types that exist so far. String
    /// conversion follows `Number()` for the common cases (trimmed text,
    /// empty string is 0, Infinity spellings); exotic forms are Phase 3
    /// approximations.
    pub fn toNumber(self: Value) f64 {
        return switch (self.tag) {
            TAG_INT => @floatFromInt(self.payload),
            TAG_FLOAT => @bitCast(@as(u64, @bitCast(self.payload))),
            TAG_BOOL => if (self.payload != 0) 1.0 else 0.0,
            TAG_NULL => 0.0,
            TAG_STRING => blk: {
                const bytes = self.asString() orelse break :blk std.math.nan(f64);
                const trimmed = std.mem.trim(u8, bytes, " \t\n\r\x0b\x0c");
                if (trimmed.len == 0) break :blk 0.0;
                if (std.mem.eql(u8, trimmed, "Infinity")) break :blk std.math.inf(f64);
                if (std.mem.eql(u8, trimmed, "+Infinity")) break :blk std.math.inf(f64);
                if (std.mem.eql(u8, trimmed, "-Infinity")) break :blk -std.math.inf(f64);
                break :blk std.fmt.parseFloat(f64, trimmed) catch std.math.nan(f64);
            },
            else => std.math.nan(f64), // undefined, objects, unknown tags
        };
    }
};

pub const Instruction = extern struct {
    op: u32,
    operand: i32,

    /// Emits the two-instruction encoding of a 64-bit float literal,
    /// mirroring `crates/r8-abi`'s `Instruction::load_f64`.
    pub fn loadF64(value: f64) [2]Instruction {
        const bits: u64 = @bitCast(value);
        return .{
            .{ .op = OP_LOAD_F64_HI, .operand = @bitCast(@as(u32, @truncate(bits >> 32))) },
            .{ .op = OP_LOAD_F64, .operand = @bitCast(@as(u32, @truncate(bits))) },
        };
    }
};

/// A borrowed string in memory owned by the value's creator; mirrors
/// `crates/r8-abi`'s `KeelString`.
pub const KeelString = extern struct {
    ptr: [*]const u8,
    len: usize,
};

/// One function's bytecode layout; mirrors `crates/r8-abi`'s `KeelFunction`.
pub const KeelFunction = extern struct {
    start: u32,
    len: u32,
    local_count: u32,
    arity: u32,
};

/// An array's element storage; mirrors `crates/r8-abi`'s `KeelArray`.
pub const KeelArray = extern struct {
    items: [*]Value,
    len: usize,
};

/// An object's parallel key/value storage with a prototype link; mirrors
/// `KeelObject`.
pub const KeelObject = extern struct {
    keys: [*]Value,
    values: [*]Value,
    len: usize,
    proto: ?*KeelObject,
};

/// A bound builtin method; mirrors `KeelMethod`.
pub const KeelMethod = extern struct {
    receiver: Value,
    id: u32,
};

comptime {
    if (@sizeOf(Value) != 16) @compileError("Value must be 16 bytes");
    if (@alignOf(Value) != 8) @compileError("Value must align to 8");
    if (@sizeOf(Instruction) != 8) @compileError("Instruction must be 8 bytes");
    if (CONFORMANCE_TAGS.len != 10) @compileError("CONFORMANCE_TAGS order must match crates/r8-abi");
    if (CONFORMANCE_OPS.len != 63) @compileError("CONFORMANCE_OPS order must match crates/r8-abi");
}

pub export fn r8_abi_version() u32 {
    return ABI_VERSION;
}

/// Conformance probes called from Rust (`r8 selftest` and a runtime test):
/// the Rust side walks its `CONFORMANCE_*` tables and compares against these,
/// proving both language halves agree on every constant and its order.
pub export fn r8_abi_tag_count() usize {
    return CONFORMANCE_TAGS.len;
}

pub export fn r8_abi_tag_at(index: usize) u32 {
    return CONFORMANCE_TAGS[index];
}

pub export fn r8_abi_op_count() usize {
    return CONFORMANCE_OPS.len;
}

pub export fn r8_abi_op_at(index: usize) u32 {
    return CONFORMANCE_OPS[index];
}

test "value round trip" {
    const value = Value.fromInt(-7);
    try std.testing.expectEqual(@as(?i64, -7), value.asInt());
}

test "value float round trip" {
    try std.testing.expectEqual(@as(?f64, 0.5), Value.fromFloat(0.5).asFloat());
    try std.testing.expectEqual(@as(?f64, std.math.inf(f64)), Value.fromFloat(std.math.inf(f64)).asFloat());
    try std.testing.expectEqual(@as(?f64, -2.75), Value.fromFloat(-2.75).asFloat());
}
