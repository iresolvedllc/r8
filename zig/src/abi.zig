const std = @import("std");

pub const ABI_VERSION: u32 = 1;

pub const TAG_INT: u32 = 1;
pub const TAG_OBJECT: u32 = 2;

pub const OP_LOAD_INT: u32 = 0;
pub const OP_ADD: u32 = 1;
pub const OP_RETURN: u32 = 2;

pub const Value = extern struct {
    tag: u32,
    _pad: u32,
    payload: i64,

    pub fn fromInt(value: i64) Value {
        return .{ .tag = TAG_INT, ._pad = 0, .payload = value };
    }

    pub fn asInt(self: Value) ?i64 {
        if (self.tag != TAG_INT) return null;
        return self.payload;
    }
};

pub const Instruction = extern struct {
    op: u32,
    operand: i32,
};

comptime {
    if (@sizeOf(Value) != 16) @compileError("Value must be 16 bytes");
    if (@alignOf(Value) != 8) @compileError("Value must align to 8");
    if (@sizeOf(Instruction) != 8) @compileError("Instruction must be 8 bytes");
}

pub export fn r8_abi_version() u32 {
    return ABI_VERSION;
}

test "value round trip" {
    const value = Value.fromInt(-7);
    try std.testing.expectEqual(@as(?i64, -7), value.asInt());
}
