const std = @import("std");
const abi = @import("abi.zig");

pub fn run(instructions: []const abi.Instruction) abi.Value {
    var stack: [256]abi.Value = undefined;
    var sp: usize = 0;

    for (instructions) |instruction| {
        switch (instruction.op) {
            abi.OP_LOAD_INT => {
                if (sp >= stack.len) @panic("stack overflow");
                stack[sp] = abi.Value.fromInt(instruction.operand);
                sp += 1;
            },
            abi.OP_ADD => {
                if (sp < 2) @panic("stack underflow");
                const right = stack[sp - 1];
                const left = stack[sp - 2];
                sp -= 2;
                const left_value = left.asInt() orelse @panic("add expects integers");
                const right_value = right.asInt() orelse @panic("add expects integers");
                stack[sp] = abi.Value.fromInt(left_value + right_value);
                sp += 1;
            },
            abi.OP_RETURN => {
                if (sp == 0) @panic("return on empty stack");
                return stack[sp - 1];
            },
            else => @panic("unknown opcode"),
        }
    }

    return abi.Value.fromInt(0);
}

pub export fn r8_machine_run(instructions: [*]const abi.Instruction, len: usize) abi.Value {
    return run(instructions[0..len]);
}

test "interpreter adds" {
    const instructions = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_ADD, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const value = run(&instructions);
    try std.testing.expectEqual(@as(?i64, 2), value.asInt());
}
