const std = @import("std");
const abi = @import("abi.zig");
const interp = @import("interp.zig");

pub fn main() !void {
    // typeof Number via compiled bytecode
    const code = [_]abi.Instruction{
        .{ .op = abi.OP_GLOBAL_GET, .operand = 0 }, // "Number"
        .{ .op = abi.OP_TYPEOF, .operand = 0 },
        .{ .op = abi.OP_PRINT, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    var name = abi.KeelString{ .ptr = "Number".ptr, .len = 6 };
    const pool = [_]abi.Value{.{
        .tag = abi.TAG_STRING,
        ._pad = 0,
        .payload = @bitCast(@as(i64, @intCast(@intFromPtr(&name)))),
    }};
    _ = interp.run(&code, &pool, &[_]abi.KeelFunction{});
}
