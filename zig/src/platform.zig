const std = @import("std");

pub fn pageSize() usize {
    return std.heap.page_size_min;
}

test "page size is non-zero" {
    try std.testing.expect(pageSize() > 0);
}
