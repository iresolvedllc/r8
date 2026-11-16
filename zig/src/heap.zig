const std = @import("std");

pub const Heap = struct {
    allocator: std.mem.Allocator,

    pub fn init(allocator: std.mem.Allocator) Heap {
        return .{ .allocator = allocator };
    }

    pub fn allocate(self: *Heap, size: usize) ![]u8 {
        return self.allocator.alloc(u8, size);
    }

    pub fn release(self: *Heap, memory: []u8) void {
        self.allocator.free(memory);
    }
};

test "heap allocates and releases" {
    var heap = Heap.init(std.testing.allocator);
    const memory = try heap.allocate(64);
    defer heap.release(memory);
    try std.testing.expectEqual(@as(usize, 64), memory.len);
}
