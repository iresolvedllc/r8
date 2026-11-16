const std = @import("std");

pub const BaselineError = error{NotImplemented};

pub fn compile(_: []const u8) BaselineError!void {
    return BaselineError.NotImplemented;
}

test "baseline is an explicit stub" {
    try std.testing.expectError(BaselineError.NotImplemented, compile(&[_]u8{}));
}
