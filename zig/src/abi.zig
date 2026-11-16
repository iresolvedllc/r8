const std = @import("std");

pub const ABI_VERSION: u32 = 2;

/// Value tags. Mirrors `crates/r8-abi`'s constants; the `CONFORMANCE_TAGS`
/// table below lists them in the same canonical order — keep both in sync.
pub const TAG_INT: u32 = 1;
pub const TAG_OBJECT: u32 = 2;
pub const TAG_FLOAT: u32 = 3;
pub const TAG_BOOL: u32 = 4;
pub const TAG_NULL: u32 = 5;
pub const TAG_UNDEFINED: u32 = 6;

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

/// Canonical tag order mirrored from `crates/r8-abi`'s `CONFORMANCE_TAGS`.
const CONFORMANCE_TAGS = [_]u32{ TAG_INT, TAG_OBJECT, TAG_FLOAT, TAG_BOOL, TAG_NULL, TAG_UNDEFINED };

/// Canonical opcode order mirrored from `crates/r8-abi`'s `CONFORMANCE_OPS`.
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

    pub fn isNumber(self: Value) bool {
        return self.tag == TAG_INT or self.tag == TAG_FLOAT;
    }

    /// JS `ToNumber` over the value types that exist so far. Strings and
    /// objects arrive in later phases.
    pub fn toNumber(self: Value) f64 {
        return switch (self.tag) {
            TAG_INT => @floatFromInt(self.payload),
            TAG_FLOAT => @bitCast(@as(u64, @bitCast(self.payload))),
            TAG_BOOL => if (self.payload != 0) 1.0 else 0.0,
            TAG_NULL => 0.0,
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

comptime {
    if (@sizeOf(Value) != 16) @compileError("Value must be 16 bytes");
    if (@alignOf(Value) != 8) @compileError("Value must align to 8");
    if (@sizeOf(Instruction) != 8) @compileError("Instruction must be 8 bytes");
    if (CONFORMANCE_TAGS.len != 6) @compileError("CONFORMANCE_TAGS order must match crates/r8-abi");
    if (CONFORMANCE_OPS.len != 25) @compileError("CONFORMANCE_OPS order must match crates/r8-abi");
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
