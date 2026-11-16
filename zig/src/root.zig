pub const abi = @import("abi.zig");
pub const interp = @import("interp.zig");
pub const heap = @import("heap.zig");
pub const baseline = @import("baseline.zig");
pub const platform = @import("platform.zig");

comptime {
    _ = abi;
    _ = interp;
}

test {
    _ = abi;
    _ = interp;
    _ = heap;
    _ = baseline;
    _ = platform;
}
