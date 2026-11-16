const std = @import("std");
const abi = @import("abi.zig");

/// The machine's string arena. Runtime strings and copies of pool constants
/// are allocated here and live until the next run resets it (a documented
/// leak-per-run until the GC phase). A spinlock guards it: the machine holds
/// global state, so runs are serialized (true isolation/isolates arrive with
/// the Phase 6 runtime work).
var string_arena: ?std.heap.ArenaAllocator = null;
var machine_lock = std.atomic.Value(u32).init(0);

fn acquireMachine() void {
    while (machine_lock.cmpxchgStrong(0, 1, .acquire, .monotonic) != null) {
        _ = std.Thread.yield() catch {};
    }
}

fn releaseMachine() void {
    machine_lock.store(0, .release);
}

fn resetStringArena() void {
    if (string_arena) |*arena| arena.deinit();
    string_arena = std.heap.ArenaAllocator.init(std.heap.page_allocator);
}

fn machineAllocator() std.mem.Allocator {
    return string_arena.?.allocator();
}

/// Allocates a string value in the machine's arena.
fn allocString(bytes: []const u8) abi.Value {
    const allocator = machineAllocator();
    const data = allocator.alloc(u8, bytes.len) catch @panic("out of memory");
    @memcpy(data, bytes);
    return allocStringFrom(data.ptr, data.len);
}

/// Wraps already-arena-owned bytes into a string value (no copy).
fn allocStringFrom(ptr: [*]const u8, len: usize) abi.Value {
    const allocator = machineAllocator();
    const keel_string = allocator.create(abi.KeelString) catch @panic("out of memory");
    keel_string.* = .{ .ptr = ptr, .len = len };
    return .{ .tag = abi.TAG_STRING, ._pad = 0, .payload = @bitCast(@intFromPtr(keel_string)) };
}

/// The bytecode machine: a stack interpreter over Keel instructions.
///
/// Numeric semantics follow JavaScript: `+ - * %` run on an integer fast
/// path and promote to doubles on overflow, float operands, or division;
/// comparisons are numeric; equality distinguishes loose from strict;
/// every value prints exactly like JS `Number::toString` / `String()`.
///
/// Locals are a flat array of slots; the compiler gives every declaration
/// its own slot (block shadowing then needs no runtime save/restore) and
/// pads slots with `undefined` so uninitialized reads are JS-shaped.

const LOCAL_COUNT = 256;

fn localSlot(operand: i32) usize {
    const slot: usize = @intCast(operand);
    if (slot >= LOCAL_COUNT) @panic("local slot out of range");
    return slot;
}

fn jumpTarget(operand: i32) usize {
    if (operand < 0) @panic("jump target out of range");
    return @intCast(operand);
}

const Stack = struct {
    values: [256]abi.Value = undefined,
    sp: usize = 0,

    fn push(self: *Stack, value: abi.Value) void {
        if (self.sp >= self.values.len) @panic("stack overflow");
        self.values[self.sp] = value;
        self.sp += 1;
    }

    fn pop(self: *Stack) abi.Value {
        if (self.sp == 0) @panic("stack underflow");
        self.sp -= 1;
        return self.values[self.sp];
    }

    fn top(self: *Stack) abi.Value {
        if (self.sp == 0) return abi.Value.UNDEF;
        return self.values[self.sp - 1];
    }
};

const Arithmetic = enum { add, sub, mul, div, mod, pow };
const Compare = enum { lt, gt, le, ge };
const Equality = enum { loose, strict };

pub fn run(instructions: []const abi.Instruction, pool: []const abi.Value) abi.Value {
    acquireMachine();
    defer releaseMachine();
    resetStringArena();
    var stack: Stack = .{};
    var locals: [LOCAL_COUNT]abi.Value = undefined;
    for (&locals) |*slot| slot.* = abi.Value.UNDEF;
    var pc: usize = 0;

    while (pc < instructions.len) {
        const instruction = instructions[pc];
        pc += 1;
        switch (instruction.op) {
            abi.OP_LOAD_INT => stack.push(abi.Value.fromInt(instruction.operand)),
            // The raw high half of a float literal, pushed as ordinary data.
            abi.OP_LOAD_F64_HI => stack.push(abi.Value.fromInt(instruction.operand)),
            abi.OP_LOAD_F64 => {
                const high = stack.pop();
                const high_bits: u64 = @as(u64, @bitCast(high.payload)) & 0xFFFF_FFFF;
                const low_bits: u64 = @as(u32, @bitCast(instruction.operand));
                stack.push(abi.Value.fromFloat(@bitCast((high_bits << 32) | low_bits)));
            },
            abi.OP_LOAD_BOOL => stack.push(abi.Value.fromBool(instruction.operand != 0)),
            abi.OP_LOAD_NULL => stack.push(abi.Value.NULL),
            abi.OP_LOAD_UNDEFINED => stack.push(abi.Value.UNDEF),

            abi.OP_ADD => binaryArithmetic(.add, &stack),
            abi.OP_SUB => binaryArithmetic(.sub, &stack),
            abi.OP_MUL => binaryArithmetic(.mul, &stack),
            abi.OP_DIV => binaryArithmetic(.div, &stack),
            abi.OP_MOD => binaryArithmetic(.mod, &stack),
            abi.OP_POW => binaryArithmetic(.pow, &stack),

            abi.OP_LT => binaryCompare(.lt, &stack),
            abi.OP_GT => binaryCompare(.gt, &stack),
            abi.OP_LE => binaryCompare(.le, &stack),
            abi.OP_GE => binaryCompare(.ge, &stack),

            abi.OP_EQ => binaryEquality(.loose, &stack),
            abi.OP_NE => {
                const right = stack.pop();
                const left = stack.pop();
                stack.push(abi.Value.fromBool(!equals(.loose, left, right)));
            },
            abi.OP_EQ_STRICT => binaryEquality(.strict, &stack),
            abi.OP_NE_STRICT => {
                const right = stack.pop();
                const left = stack.pop();
                stack.push(abi.Value.fromBool(!equals(.strict, left, right)));
            },

            abi.OP_NEG => stack.push(negate(stack.pop())),
            abi.OP_NOT => {
                const value = stack.pop();
                stack.push(abi.Value.fromBool(!isTruthy(value)));
            },

            abi.OP_PRINT => {
                // console.log intrinsic: pop, print, leave `undefined` behind
                // so every expression still leaves exactly one value.
                const value = stack.pop();
                printValue(value);
                stack.push(abi.Value.UNDEF);
            },
            abi.OP_POP => _ = stack.pop(),

            abi.OP_DUP => {
                const value = stack.pop();
                stack.push(value);
                stack.push(value);
            },
            abi.OP_GET_LOCAL => stack.push(locals[localSlot(instruction.operand)]),
            abi.OP_SET_LOCAL => locals[localSlot(instruction.operand)] = stack.pop(),

            abi.OP_JMP => pc = jumpTarget(instruction.operand),
            abi.OP_JMP_IF_FALSE => {
                const condition = stack.pop();
                if (!isTruthy(condition)) pc = jumpTarget(instruction.operand);
            },
            abi.OP_JMP_IF_TRUE => {
                const condition = stack.pop();
                if (isTruthy(condition)) pc = jumpTarget(instruction.operand);
            },

            abi.OP_LOAD_STRING => {
                const index: usize = @intCast(instruction.operand);
                if (index >= pool.len) @panic("string pool out of range");
                const constant = pool[index];
                if (constant.tag != abi.TAG_STRING) @panic("pool entry is not a string");
                // Copy the pool bytes into the arena; the pool only needs to
                // outlive the call.
                stack.push(allocString(constant.asString().?));
            },
            abi.OP_TO_STRING => {
                const value = stack.pop();
                if (value.tag == abi.TAG_STRING) {
                    stack.push(value);
                } else {
                    stack.push(allocString(stringBytesOf(value)));
                }
            },

            abi.OP_RETURN => return stack.top(),
            else => @panic("unknown opcode"),
        }
    }

    return stack.top();
}

pub export fn r8_machine_run(
    instructions: [*]const abi.Instruction,
    len: usize,
    pool: [*]const abi.Value,
    pool_len: usize,
) abi.Value {
    return run(instructions[0..len], pool[0..pool_len]);
}

fn binaryArithmetic(kind: Arithmetic, stack: *Stack) void {
    const right = stack.pop();
    const left = stack.pop();
    stack.push(arithmetic(kind, left, right));
}

fn arithmetic(kind: Arithmetic, left: abi.Value, right: abi.Value) abi.Value {
    switch (kind) {
        .add, .sub, .mul => {
            // JS `+` concatenates when either operand is a string.
            if (kind == .add and (left.tag == abi.TAG_STRING or right.tag == abi.TAG_STRING)) {
                return concatValues(left, right);
            }
            if (left.tag == abi.TAG_INT and right.tag == abi.TAG_INT) {
                const result = switch (kind) {
                    .add => @addWithOverflow(left.payload, right.payload),
                    .sub => @subWithOverflow(left.payload, right.payload),
                    .mul => @mulWithOverflow(left.payload, right.payload),
                    else => unreachable,
                };
                if (result[1] == 0) return abi.Value.fromInt(result[0]);
            }
            const x = left.toNumber();
            const y = right.toNumber();
            return abi.Value.fromFloat(switch (kind) {
                .add => x + y,
                .sub => x - y,
                .mul => x * y,
                else => unreachable,
            });
        },
        // JS division always produces a double: 4 / 2 is 2.0, 1 / 0 is
        // Infinity, 0 / 0 is NaN — all of which fall out of IEEE semantics.
        .div => return abi.Value.fromFloat(left.toNumber() / right.toNumber()),
        .mod => {
            if (left.tag == abi.TAG_INT and right.tag == abi.TAG_INT and
                right.payload != 0 and
                !(left.payload == std.math.minInt(i64) and right.payload == -1))
            {
                return abi.Value.fromInt(@rem(left.payload, right.payload));
            }
            return abi.Value.fromFloat(@rem(left.toNumber(), right.toNumber()));
        },
        .pow => return abi.Value.fromFloat(std.math.pow(f64, left.toNumber(), right.toNumber())),
    }
}

/// JS string coercion: strings pass through, everything else renders like
/// `String()`. The returned bytes are either borrowed from the value's own
/// memory or freshly arena-allocated.
fn stringBytesOf(value: abi.Value) []const u8 {
    if (value.tag == abi.TAG_STRING) return value.asString().?;
    var buffer: [96]u8 = undefined;
    const formatted = formatValue(value, &buffer) catch @panic("string coercion failed");
    return machineAllocator().dupe(u8, formatted) catch @panic("out of memory");
}

fn concatValues(left: abi.Value, right: abi.Value) abi.Value {
    const left_bytes = stringBytesOf(left);
    const right_bytes = stringBytesOf(right);
    const allocator = machineAllocator();
    const data = allocator.alloc(u8, left_bytes.len + right_bytes.len) catch @panic("out of memory");
    @memcpy(data[0..left_bytes.len], left_bytes);
    @memcpy(data[left_bytes.len..], right_bytes);
    return allocStringFrom(data.ptr, data.len);
}

fn binaryCompare(kind: Compare, stack: *Stack) void {
    const right = stack.pop();
    const left = stack.pop();
    stack.push(abi.Value.fromBool(compare(kind, left, right)));
}

fn compare(kind: Compare, left: abi.Value, right: abi.Value) bool {
    // Exact integer path keeps precision beyond 2^53; the double path gives
    // IEEE semantics, where NaN compares false against everything. Strings
    // compare bytewise (ASCII-exact; UTF-16 code-unit order comes later).
    if (left.tag == abi.TAG_INT and right.tag == abi.TAG_INT) {
        return switch (kind) {
            .lt => left.payload < right.payload,
            .gt => left.payload > right.payload,
            .le => left.payload <= right.payload,
            .ge => left.payload >= right.payload,
        };
    }
    if (left.tag == abi.TAG_STRING and right.tag == abi.TAG_STRING) {
        const order = std.mem.order(u8, left.asString().?, right.asString().?);
        return switch (kind) {
            .lt => order == .lt,
            .gt => order == .gt,
            .le => order != .gt,
            .ge => order != .lt,
        };
    }
    const x = left.toNumber();
    const y = right.toNumber();
    return switch (kind) {
        .lt => x < y,
        .gt => x > y,
        .le => x <= y,
        .ge => x >= y,
    };
}

fn binaryEquality(kind: Equality, stack: *Stack) void {
    const right = stack.pop();
    const left = stack.pop();
    stack.push(abi.Value.fromBool(equals(kind, left, right)));
}

fn equals(kind: Equality, left: abi.Value, right: abi.Value) bool {
    if (left.isNumber() and right.isNumber()) {
        if (left.tag == abi.TAG_INT and right.tag == abi.TAG_INT) return left.payload == right.payload;
        // IEEE double equality: NaN is unequal to itself, +0 equals -0, and
        // 2 and 2.0 are the same JS number.
        return left.toNumber() == right.toNumber();
    }
    if (left.tag == abi.TAG_STRING and right.tag == abi.TAG_STRING) {
        return std.mem.eql(u8, left.asString().?, right.asString().?);
    }
    switch (kind) {
        .strict => {
            if (left.tag != right.tag) return false;
            return left.payload == right.payload;
        },
        .loose => {
            // `null == undefined` and both equal nothing else (so far).
            const l_nullish = left.tag == abi.TAG_NULL or left.tag == abi.TAG_UNDEFINED;
            const r_nullish = right.tag == abi.TAG_NULL or right.tag == abi.TAG_UNDEFINED;
            if (l_nullish or r_nullish) return l_nullish and r_nullish;
            // Booleans coerce to numbers under loose equality.
            if (left.tag == abi.TAG_BOOL) return equals(.loose, abi.Value.fromFloat(left.toNumber()), right);
            if (right.tag == abi.TAG_BOOL) return equals(.loose, left, abi.Value.fromFloat(right.toNumber()));
            // Strings compare numerically against numbers ("5" == 5).
            if (left.tag == abi.TAG_STRING and right.isNumber()) return left.toNumber() == right.toNumber();
            if (right.tag == abi.TAG_STRING and left.isNumber()) return right.toNumber() == left.toNumber();
            return false;
        },
    }
}

fn isTruthy(value: abi.Value) bool {
    return switch (value.tag) {
        abi.TAG_INT => value.payload != 0,
        abi.TAG_FLOAT => blk: {
            const f: f64 = @bitCast(@as(u64, @bitCast(value.payload)));
            break :blk f != 0 and !std.math.isNan(f);
        },
        abi.TAG_BOOL => value.payload != 0,
        abi.TAG_STRING => value.asString().?.len != 0,
        abi.TAG_NULL, abi.TAG_UNDEFINED => false,
        else => true, // objects are truthy; unknown tags err truthy
    };
}

fn negate(value: abi.Value) abi.Value {
    switch (value.tag) {
        abi.TAG_INT => {
            // JS has no negative zero integers: -0 is a double.
            if (value.payload == 0) return abi.Value.fromFloat(-0.0);
            if (value.payload == std.math.minInt(i64)) {
                return abi.Value.fromFloat(-@as(f64, @floatFromInt(value.payload)));
            }
            return abi.Value.fromInt(-value.payload);
        },
        else => return abi.Value.fromFloat(-value.toNumber()),
    }
}

fn printValue(value: abi.Value) void {
    const io = std.Options.debug_io;
    var buffer: [256]u8 = undefined;
    var file_writer = std.Io.File.stdout().writer(io, &buffer);
    const writer = &file_writer.interface;
    // Strings are written directly: they can exceed any fixed buffer.
    if (value.tag == abi.TAG_STRING) {
        writer.writeAll(value.asString() orelse "<string>") catch {};
    } else {
        var text: [96]u8 = undefined;
        const formatted = formatValue(value, &text) catch return;
        writer.writeAll(formatted) catch {};
    }
    writer.writeAll("\n") catch {};
    writer.flush() catch {};
}

/// Formats a value the way JS `String()` does. Used by the `console.log`
/// intrinsic and unit-tested directly; `buffer` must hold the longest
/// possible rendering (96 bytes covers every current type).
pub fn formatValue(value: abi.Value, buffer: []u8) ![]const u8 {
    return switch (value.tag) {
        abi.TAG_INT => std.fmt.bufPrint(buffer, "{d}", .{value.payload}),
        abi.TAG_FLOAT => formatJsNumber(@bitCast(@as(u64, @bitCast(value.payload))), buffer),
        abi.TAG_BOOL => if (value.payload != 0) "true" else "false",
        abi.TAG_NULL => "null",
        abi.TAG_UNDEFINED => "undefined",
        abi.TAG_STRING => blk: {
            const bytes = value.asString() orelse break :blk "<string>";
            if (bytes.len > buffer.len) break :blk error.StringTooLong;
            @memcpy(buffer[0..bytes.len], bytes);
            break :blk buffer[0..bytes.len];
        },
        abi.TAG_OBJECT => "[object]",
        else => std.fmt.bufPrint(buffer, "<value tag={d}>", .{value.tag}),
    };
}

/// JS `Number::toString`: shortest round-trip digits, fixed notation for
/// exponents in (-6, 21), scientific notation otherwise (`1e+21`, `1e-7`),
/// `NaN`/`Infinity`, and `"0"` for both signed zeros.
fn formatJsNumber(x: f64, buffer: []u8) ![]const u8 {
    if (std.math.isNan(x)) return "NaN";
    if (x == 0.0) return "0";
    if (x < 0) {
        buffer[0] = '-';
        const rest = try formatJsNumber(-x, buffer[1..]);
        // Copy only when `rest` is a static literal ("Infinity"); when it is
        // buffer-backed it already sits right after the '-'.
        if (rest.ptr != buffer[1..].ptr) {
            @memcpy(buffer[1..][0..rest.len], rest);
        }
        return buffer[0 .. 1 + rest.len];
    }
    if (std.math.isInf(x)) return "Infinity";
    // `{e}` yields the shortest round-trip representation in scientific
    // form, e.g. `3.0000000000000004e-1`. Split it into digits and exponent.
    var scientific: [64]u8 = undefined;
    const text = try std.fmt.bufPrint(&scientific, "{e}", .{x});
    const e_index = std.mem.indexOfScalar(u8, text, 'e') orelse return error.MalformedNumber;
    const exponent = std.fmt.parseInt(i32, text[e_index + 1 ..], 10) catch return error.MalformedNumber;
    var digits: [24]u8 = undefined;
    var count: usize = 0;
    for (text[0..e_index]) |c| {
        if (c >= '0' and c <= '9') {
            digits[count] = c;
            count += 1;
        }
    }
    return formatJsNumberDigits(buffer, digits[0..count], exponent + 1);
}

/// Emits digits `d1..dk` where the value is `d1.d2..dk × 10^(n-1)`, following
/// the notation rules of ECMA-262 `Number::toString`.
fn formatJsNumberDigits(out: []u8, digits: []const u8, n: i32) ![]const u8 {
    const k: i32 = @intCast(digits.len);
    var w: usize = 0;
    if (k <= n and n <= 21) {
        @memcpy(out[w..][0..digits.len], digits);
        w += digits.len;
        var i: i32 = 0;
        while (i < n - k) : (i += 1) {
            out[w] = '0';
            w += 1;
        }
    } else if (0 < n and n <= 21) {
        const point: usize = @intCast(n);
        @memcpy(out[w..][0..point], digits[0..point]);
        w += point;
        out[w] = '.';
        w += 1;
        @memcpy(out[w..][0 .. digits.len - point], digits[point..]);
        w += digits.len - point;
    } else if (-6 < n and n <= 0) {
        out[w] = '0';
        w += 1;
        out[w] = '.';
        w += 1;
        var i: i32 = 0;
        while (i < -n) : (i += 1) {
            out[w] = '0';
            w += 1;
        }
        @memcpy(out[w..][0..digits.len], digits);
        w += digits.len;
    } else {
        out[w] = digits[0];
        w += 1;
        if (k > 1) {
            out[w] = '.';
            w += 1;
            @memcpy(out[w..][0 .. digits.len - 1], digits[1..]);
            w += digits.len - 1;
        }
        const exponent = n - 1;
        out[w] = 'e';
        w += 1;
        out[w] = if (exponent < 0) '-' else '+';
        w += 1;
        const exp_abs: u32 = @intCast(@abs(exponent));
        var exp_text: [8]u8 = undefined;
        const printed = std.fmt.bufPrint(&exp_text, "{d}", .{exp_abs}) catch unreachable;
        @memcpy(out[w..][0..printed.len], printed);
        w += printed.len;
    }
    return out[0..w];
}

test "interpreter adds" {
    const instructions = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_ADD, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const value = run(&instructions, &[_]abi.Value{});
    try std.testing.expectEqual(@as(?i64, 2), value.asInt());
}

test "arithmetic promotes and follows js semantics" {
    const expectInt = struct {
        fn call(instructions: []const abi.Instruction, expected: i64) !void {
            const value = run(instructions, &[_]abi.Value{});
            try std.testing.expectEqual(@as(?i64, expected), value.asInt());
        }
    }.call;
    const expectFloat = struct {
        fn call(instructions: []const abi.Instruction, expected: f64) !void {
            const value = run(instructions, &[_]abi.Value{});
            const actual = value.asFloat() orelse return error.NotAFloat;
            if (std.math.isNan(expected)) {
                // NaN has many bit patterns; only its class is observable.
                try std.testing.expect(std.math.isNan(actual));
            } else {
                // Compare by bit pattern so ±0 and exact doubles match.
                try std.testing.expectEqual(@as(u64, @bitCast(expected)), @as(u64, @bitCast(actual)));
            }
        }
    }.call;

    const build = struct {
        fn build(left: i64, op: u32, right: i64) [4]abi.Instruction {
            return .{
                .{ .op = abi.OP_LOAD_INT, .operand = @intCast(left) },
                .{ .op = abi.OP_LOAD_INT, .operand = @intCast(right) },
                .{ .op = op, .operand = 0 },
                .{ .op = abi.OP_RETURN, .operand = 0 },
            };
        }
    }.build;

    try expectInt(&build(7, abi.OP_SUB, 3), 4);
    try expectInt(&build(-4, abi.OP_MUL, 6), -24);
    try expectInt(&build(7, abi.OP_MOD, 3), 1);
    try expectInt(&build(-7, abi.OP_MOD, 3), -1); // sign follows the dividend
    try expectFloat(&build(4, abi.OP_DIV, 2), 2.0);
    try expectFloat(&build(1, abi.OP_DIV, 2), 0.5);
    try expectFloat(&build(1, abi.OP_DIV, 0), std.math.inf(f64));
    try expectFloat(&build(0, abi.OP_DIV, 0), std.math.nan(f64));
    try expectFloat(&build(7, abi.OP_MOD, 0), std.math.nan(f64));

    // Integer overflow promotes to double, exactly like JS. (Not reachable
    // through i32-bounded literals today; tested on the arithmetic helper
    // directly for when larger loads exist.)
    const huge = abi.Value.fromInt(std.math.maxInt(i64));
    const sum = arithmetic(.add, huge, huge);
    try std.testing.expectEqual(@as(f64, 2.0 * 9.223372036854775807e18), sum.asFloat().?);
    const product = arithmetic(.mul, huge, abi.Value.fromInt(2));
    // (2^63) × 2 is finite in doubles — 2^64, exactly like JS.
    try std.testing.expectEqual(@as(f64, 1.8446744073709552e19), product.asFloat().?);

    // Power is computed in doubles, matching JS Math.pow.
    const pow = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_INT, .operand = 2 },
        .{ .op = abi.OP_LOAD_INT, .operand = 10 },
        .{ .op = abi.OP_POW, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    try expectFloat(&pow, 1024.0);
}

test "comparisons and equality" {
    const run2 = struct {
        fn call(op: u32, left: i64, right: i64) bool {
            const instructions = [_]abi.Instruction{
                .{ .op = abi.OP_LOAD_INT, .operand = @intCast(left) },
                .{ .op = abi.OP_LOAD_INT, .operand = @intCast(right) },
                .{ .op = op, .operand = 0 },
                .{ .op = abi.OP_RETURN, .operand = 0 },
            };
            return run(&instructions, &[_]abi.Value{}).asBool().?;
        }
    }.call;

    try std.testing.expect(run2(abi.OP_LT, 1, 2));
    try std.testing.expect(!run2(abi.OP_LT, 2, 2));
    try std.testing.expect(run2(abi.OP_LE, 2, 2));
    try std.testing.expect(run2(abi.OP_GT, 3, 2));
    try std.testing.expect(run2(abi.OP_GE, 2, 2));

    try std.testing.expect(run2(abi.OP_EQ, 2, 2));
    try std.testing.expect(!run2(abi.OP_EQ, 2, 3));
    try std.testing.expect(!run2(abi.OP_NE, 2, 2));
    try std.testing.expect(run2(abi.OP_EQ_STRICT, 2, 2));
    try std.testing.expect(run2(abi.OP_NE_STRICT, 2, 3));

    // null/undefined loose equality table.
    const loose = struct {
        fn call(left_op: u32, right_op: u32, op: u32) bool {
            const instructions = [_]abi.Instruction{
                .{ .op = left_op, .operand = 0 },
                .{ .op = right_op, .operand = 0 },
                .{ .op = op, .operand = 0 },
                .{ .op = abi.OP_RETURN, .operand = 0 },
            };
            return run(&instructions, &[_]abi.Value{}).asBool().?;
        }
    }.call;
    try std.testing.expect(loose(abi.OP_LOAD_NULL, abi.OP_LOAD_UNDEFINED, abi.OP_EQ));
    try std.testing.expect(!loose(abi.OP_LOAD_NULL, abi.OP_LOAD_UNDEFINED, abi.OP_EQ_STRICT));
    try std.testing.expect(!loose(abi.OP_LOAD_NULL, abi.OP_LOAD_INT, abi.OP_EQ)); // null == 0 is false
    try std.testing.expect(loose(abi.OP_LOAD_NULL, abi.OP_LOAD_NULL, abi.OP_EQ));
}

test "unary neg and not" {
    const neg_zero = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_INT, .operand = 0 },
        .{ .op = abi.OP_NEG, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const negated = run(&neg_zero, &[_]abi.Value{});
    try std.testing.expectEqual(@as(f64, -0.0), negated.asFloat().?);

    const not_zero = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_INT, .operand = 0 },
        .{ .op = abi.OP_NOT, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    try std.testing.expect(run(&not_zero, &[_]abi.Value{}).asBool().?);

    const not_one = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_NOT, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    try std.testing.expect(!run(&not_one, &[_]abi.Value{}).asBool().?);
}

test "float literals load through the two-instruction encoding" {
    const load = abi.Instruction.loadF64(0.30000000000000004);
    const instructions = [_]abi.Instruction{ load[0], load[1], .{ .op = abi.OP_RETURN, .operand = 0 } };
    const value = run(&instructions, &[_]abi.Value{});
    try std.testing.expectEqual(@as(?f64, 0.30000000000000004), value.asFloat());
}

test "print pops and leaves undefined" {
    const instructions = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_INT, .operand = 5 },
        .{ .op = abi.OP_PRINT, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const value = run(&instructions, &[_]abi.Value{});
    try std.testing.expectEqual(abi.TAG_UNDEFINED, value.tag);
}

test "locals round trip and default to undefined" {
    const set_get = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_INT, .operand = 5 },
        .{ .op = abi.OP_SET_LOCAL, .operand = 0 },
        .{ .op = abi.OP_GET_LOCAL, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    try std.testing.expectEqual(@as(?i64, 5), run(&set_get, &[_]abi.Value{}).asInt());

    const read_default = [_]abi.Instruction{
        .{ .op = abi.OP_GET_LOCAL, .operand = 7 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const value = run(&read_default, &[_]abi.Value{});
    try std.testing.expectEqual(abi.TAG_UNDEFINED, value.tag);
}

test "dup duplicates the top of the stack" {
    const instructions = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_DUP, .operand = 0 },
        .{ .op = abi.OP_ADD, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    try std.testing.expectEqual(@as(?i64, 2), run(&instructions, &[_]abi.Value{}).asInt());
}

test "jumps control the program counter" {
    // Skip the poison value.
    const jmp = [_]abi.Instruction{
        .{ .op = abi.OP_JMP, .operand = 3 },
        .{ .op = abi.OP_LOAD_INT, .operand = 99 },
        .{ .op = abi.OP_POP, .operand = 0 },
        .{ .op = abi.OP_LOAD_INT, .operand = 7 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    try std.testing.expectEqual(@as(?i64, 7), run(&jmp, &[_]abi.Value{}).asInt());

    // Conditional jump taken on a falsy condition only.
    const branch = struct {
        fn call(condition: i64, expected: i64) !void {
            const instructions = [_]abi.Instruction{
                .{ .op = abi.OP_LOAD_INT, .operand = @intCast(condition) },
                .{ .op = abi.OP_JMP_IF_FALSE, .operand = 4 },
                .{ .op = abi.OP_LOAD_INT, .operand = 1 },
                .{ .op = abi.OP_JMP, .operand = 5 },
                .{ .op = abi.OP_LOAD_INT, .operand = 0 },
                .{ .op = abi.OP_RETURN, .operand = 0 },
            };
            try std.testing.expectEqual(@as(?i64, expected), run(&instructions, &[_]abi.Value{}).asInt());
        }
    }.call;
    try branch(1, 1); // truthy: falls through
    try branch(0, 0); // falsy: jumps
}

test "string values round trip through run and pool" {
    // Build a pool the way the Rust front end does: KeelString pointers.
    const text = "ab";
    var keel_string = abi.KeelString{ .ptr = text.ptr, .len = text.len };
    const pool = [_]abi.Value{.{
        .tag = abi.TAG_STRING,
        ._pad = 0,
        .payload = @bitCast(@as(i64, @intCast(@intFromPtr(&keel_string)))),
    }};
    const instructions = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_STRING, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const value = run(&instructions, &pool);
    try std.testing.expectEqualStrings("ab", value.asString().?);
}

test "add concatenates when either operand is a string" {
    const text_a = "a";
    var keel_string_a = abi.KeelString{ .ptr = text_a.ptr, .len = text_a.len };
    const pool = [_]abi.Value{.{
        .tag = abi.TAG_STRING,
        ._pad = 0,
        .payload = @bitCast(@as(i64, @intCast(@intFromPtr(&keel_string_a)))),
    }};
    const instructions = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_STRING, .operand = 0 },
        .{ .op = abi.OP_LOAD_INT, .operand = 5 },
        .{ .op = abi.OP_ADD, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const value = run(&instructions, &pool);
    try std.testing.expectEqualStrings("a5", value.asString().?);
}

test "to_string renders template substitutions" {
    // 0.5 -> "0.5"
    const load = abi.Instruction.loadF64(0.5);
    const program = [_]abi.Instruction{ load[0], load[1], .{ .op = abi.OP_TO_STRING, .operand = 0 }, .{ .op = abi.OP_RETURN, .operand = 0 } };
    const value = run(&program, &[_]abi.Value{});
    try std.testing.expectEqualStrings("0.5", value.asString().?);
}

test "string comparisons and equality follow js rules" {
    const call = struct {
        fn call(op: u32, left: []const u8, right: []const u8) bool {
            var keel_left = abi.KeelString{ .ptr = left.ptr, .len = left.len };
            var keel_right = abi.KeelString{ .ptr = right.ptr, .len = right.len };
            const pool = [_]abi.Value{
                .{ .tag = abi.TAG_STRING, ._pad = 0, .payload = @bitCast(@as(i64, @intCast(@intFromPtr(&keel_left)))) },
                .{ .tag = abi.TAG_STRING, ._pad = 0, .payload = @bitCast(@as(i64, @intCast(@intFromPtr(&keel_right)))) },
            };
            const instructions = [_]abi.Instruction{
                .{ .op = abi.OP_LOAD_STRING, .operand = 0 },
                .{ .op = abi.OP_LOAD_STRING, .operand = 1 },
                .{ .op = op, .operand = 0 },
                .{ .op = abi.OP_RETURN, .operand = 0 },
            };
            return run(&instructions, &pool).asBool().?;
        }
    }.call;

    try std.testing.expect(call(abi.OP_LT, "a", "b"));
    try std.testing.expect(!call(abi.OP_LT, "b", "a"));
    try std.testing.expect(call(abi.OP_LE, "a", "a"));
    try std.testing.expect(call(abi.OP_EQ_STRICT, "x", "x"));
    try std.testing.expect(!call(abi.OP_EQ_STRICT, "x", "y"));
}

test "string truthiness and numeric coercion" {
    const truthy = struct {
        fn call(bytes: []const u8) bool {
            var keel_string = abi.KeelString{ .ptr = bytes.ptr, .len = bytes.len };
            const pool = [_]abi.Value{.{
                .tag = abi.TAG_STRING,
                ._pad = 0,
                .payload = @bitCast(@as(i64, @intCast(@intFromPtr(&keel_string)))),
            }};
            const instructions = [_]abi.Instruction{
                .{ .op = abi.OP_LOAD_STRING, .operand = 0 },
                .{ .op = abi.OP_NOT, .operand = 0 },
                .{ .op = abi.OP_RETURN, .operand = 0 },
            };
            return !run(&instructions, &pool).asBool().?;
        }
    }.call;
    try std.testing.expect(truthy("0")); // JS: "0" is truthy
    try std.testing.expect(truthy("x"));
    try std.testing.expect(!truthy(""));

    // Number("5") == 5 under loose equality; strict says false.
    const loose = struct {
        fn call(bytes: []const u8) bool {
            var keel_string = abi.KeelString{ .ptr = bytes.ptr, .len = bytes.len };
            const pool = [_]abi.Value{.{
                .tag = abi.TAG_STRING,
                ._pad = 0,
                .payload = @bitCast(@as(i64, @intCast(@intFromPtr(&keel_string)))),
            }};
            const instructions = [_]abi.Instruction{
                .{ .op = abi.OP_LOAD_STRING, .operand = 0 },
                .{ .op = abi.OP_LOAD_INT, .operand = 5 },
                .{ .op = abi.OP_EQ, .operand = 0 },
                .{ .op = abi.OP_RETURN, .operand = 0 },
            };
            return run(&instructions, &pool).asBool().?;
        }
    }.call;
    try std.testing.expect(loose("5"));
    try std.testing.expect(!loose("6"));
}

test "formatValue matches js string conversion" {
    const expect = struct {
        fn call(value: abi.Value, expected: []const u8) !void {
            var buffer: [128]u8 = undefined;
            const text = try formatValue(value, &buffer);
            try std.testing.expectEqualStrings(expected, text);
        }
    }.call;

    try expect(abi.Value.fromInt(42), "42");
    try expect(abi.Value.fromInt(-7), "-7");
    try expect(abi.Value.fromBool(true), "true");
    try expect(abi.Value.fromBool(false), "false");
    try expect(abi.Value.NULL, "null");
    try expect(abi.Value.UNDEF, "undefined");

    // Number formatting vectors shared with the Rust side's tests.
    const cases = [_]struct { value: f64, expected: []const u8 }{
        .{ .value = 0.0, .expected = "0" },
        .{ .value = -0.0, .expected = "0" },
        .{ .value = 1.0, .expected = "1" },
        .{ .value = -7.0, .expected = "-7" },
        .{ .value = 100.0, .expected = "100" },
        .{ .value = 0.5, .expected = "0.5" },
        .{ .value = 0.30000000000000004, .expected = "0.30000000000000004" },
        .{ .value = 1.0 / 3.0, .expected = "0.3333333333333333" },
        .{ .value = 2.0 / 3.0, .expected = "0.6666666666666666" },
        .{ .value = 123456789.25, .expected = "123456789.25" },
        .{ .value = 0.0001, .expected = "0.0001" },
        .{ .value = 1e-6, .expected = "0.000001" },
        .{ .value = 1e-7, .expected = "1e-7" },
        .{ .value = 1e21, .expected = "1e+21" },
        .{ .value = 1e23, .expected = "1e+23" },
        .{ .value = 1e20, .expected = "100000000000000000000" },
        .{ .value = 9007199254740992.0, .expected = "9007199254740992" },
        .{ .value = 1.8446744073709552e19, .expected = "18446744073709552000" },
        .{ .value = std.math.inf(f64), .expected = "Infinity" },
        .{ .value = -std.math.inf(f64), .expected = "-Infinity" },
        .{ .value = std.math.nan(f64), .expected = "NaN" },
    };
    for (cases) |case| try expect(abi.Value.fromFloat(case.value), case.expected);
}
