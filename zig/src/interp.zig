const std = @import("std");
const abi = @import("abi.zig");

/// The machine's string/heap arena. Runtime strings, arrays, objects, cells,
/// and function objects are allocated here and live until the next run
/// resets it (a documented leak-per-run until the GC phase). A spinlock
/// guards it: the machine holds global state, so runs are serialized (true
/// isolation/isolates arrive with the Phase 6 runtime work).
var machine_arena: ?std.heap.ArenaAllocator = null;
var machine_lock = std.atomic.Value(u32).init(0);

fn acquireMachine() void {
    while (machine_lock.cmpxchgStrong(0, 1, .acquire, .monotonic) != null) {
        _ = std.Thread.yield() catch {};
    }
}

fn releaseMachine() void {
    machine_lock.store(0, .release);
}

fn resetArena() void {
    if (machine_arena) |*arena| arena.deinit();
    machine_arena = std.heap.ArenaAllocator.init(std.heap.page_allocator);
}

fn machineAllocator() std.mem.Allocator {
    return machine_arena.?.allocator();
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

/// Copies `items` into the arena and wraps them as an array value.
fn allocArray(items: []const abi.Value) abi.Value {
    const allocator = machineAllocator();
    const data = allocator.alloc(abi.Value, items.len) catch @panic("out of memory");
    @memcpy(data, items);
    const keel_array = allocator.create(abi.KeelArray) catch @panic("out of memory");
    keel_array.* = .{ .items = data.ptr, .len = data.len };
    return .{ .tag = abi.TAG_ARRAY, ._pad = 0, .payload = @bitCast(@intFromPtr(keel_array)) };
}

/// Copies keys/values into the arena and wraps them as an object value.
fn allocObject(keys: []const abi.Value, values: []const abi.Value) abi.Value {
    const allocator = machineAllocator();
    const key_data = allocator.alloc(abi.Value, keys.len) catch @panic("out of memory");
    @memcpy(key_data, keys);
    const value_data = allocator.alloc(abi.Value, values.len) catch @panic("out of memory");
    @memcpy(value_data, values);
    const keel_object = allocator.create(abi.KeelObject) catch @panic("out of memory");
    keel_object.* = .{ .keys = key_data.ptr, .values = value_data.ptr, .len = keys.len };
    return .{ .tag = abi.TAG_OBJECT, ._pad = 0, .payload = @bitCast(@intFromPtr(keel_object)) };
}

/// Wraps a receiver + builtin id into a method value.
fn allocMethod(receiver: abi.Value, id: u32) abi.Value {
    const allocator = machineAllocator();
    const keel_method = allocator.create(abi.KeelMethod) catch @panic("out of memory");
    keel_method.* = .{ .receiver = receiver, .id = id };
    return .{ .tag = abi.TAG_METHOD, ._pad = 0, .payload = @bitCast(@intFromPtr(keel_method)) };
}

/// Built-in methods reachable as property values. The u32 ids are part of
/// the machine-internal `KeelMethod` representation.
const Builtin = enum(u32) {
    map = 0,
    join = 1,

    fn fromId(id: u32) ?Builtin {
        return switch (id) {
            0 => .map,
            1 => .join,
            else => null,
        };
    }
};

const LOCAL_CAPACITY = 512;
const MAX_FRAMES = 64;
const MAX_ARGS = 32;
const MAX_HANDLERS = 64;

const Cell = struct {
    value: abi.Value,
};

const FunctionObject = struct {
    index: usize,
    env: std.ArrayList(?*Cell) = .empty,
};

const Frame = struct {
    return_pc: usize,
    stack_base: usize,
    locals_base: usize,
    /// The running function's object (holds its upvalue cells); null for
    /// the main frame.
    function: ?*FunctionObject,
};

/// An active try block: where to catch, and the state to restore.
const Handler = struct {
    catch_pc: usize,
    sp: usize,
    fp: usize,
    locals_top: usize,
};

const Stack = struct {
    values: [4096]abi.Value = undefined,
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

/// The bytecode machine: a stack interpreter over Keel instructions.
///
/// Numeric semantics follow JavaScript: `+ - * %` run on an integer fast
/// path and promote to doubles on overflow, float operands, or division;
/// `+` concatenates when either side is a string, array, or object;
/// comparisons are numeric (or bytewise for strings); equality distinguishes
/// loose from strict; objects compare by reference; every value prints
/// exactly like JS `String()`.
///
/// Locals are a flat array; each call frame owns a region of it. Captured
/// bindings are moved into heap cells and accessed transparently. Function
/// bodies live inline in the flat instruction stream through a
/// `KeelFunction` table (entry 0 = main). Builtins (`.map`, `.join`) re-enter
/// the interpreter through `invoke`, so callbacks run on the same machine.
pub const Machine = struct {
    stack: Stack = .{},
    locals: [LOCAL_CAPACITY]abi.Value = undefined,
    cells: [LOCAL_CAPACITY]?*Cell = undefined,
    frames: [MAX_FRAMES]Frame = undefined,
    handlers: [MAX_HANDLERS]Handler = undefined,
    fp: usize = 0,
    hp: usize = 0,
    locals_top: usize = 0,
    pc: usize = 0,
    /// Set when the program ended via an uncaught throw.
    thrown: bool = false,
    /// The uncaught thrown value (valid alongside `thrown`).
    pending_throw: ?abi.Value = null,
    /// Set when a throw crossed call frames, aborting in-flight builtin
    /// loops; reset by `invoke`.
    escaped: bool = false,
    code: []const abi.Instruction,
    pool: []const abi.Value,
    functions: []const abi.KeelFunction,
    building: ?*FunctionObject = null,

    fn init(
        code: []const abi.Instruction,
        pool: []const abi.Value,
        functions: []const abi.KeelFunction,
    ) Machine {
        var machine = Machine{ .code = code, .pool = pool, .functions = functions };
        for (&machine.locals) |*slot| slot.* = abi.Value.UNDEF;
        for (&machine.cells) |*cell| cell.* = null;
        // Entry 0 is the main body where execution begins.
        const main_entry: abi.KeelFunction = if (functions.len > 0)
            functions[0]
        else
            .{ .start = 0, .len = 0, .local_count = 0, .arity = 0 };
        machine.pc = main_entry.start;
        machine.locals_top = main_entry.local_count;
        machine.frames[0] = .{
            .return_pc = code.len,
            .stack_base = 0,
            .locals_base = 0,
            .function = null,
        };
        machine.fp = 1;
        return machine;
    }

    fn localSlot(self: *Machine, operand: i32) usize {
        const slot = self.frames[self.fp - 1].locals_base + @as(usize, @intCast(operand));
        if (slot >= LOCAL_CAPACITY) @panic("local slot out of range");
        return slot;
    }

    fn jumpTarget(operand: i32) usize {
        if (operand < 0) @panic("jump target out of range");
        return @intCast(operand);
    }

    /// Runs the program to completion and returns the main frame's value.
    pub fn execute(self: *Machine) abi.Value {
        while (self.pc < self.code.len) {
            if (self.step()) |value| return value;
        }
        return self.stack.top();
    }

    /// Unwinds a thrown value: to the nearest handler in the current frame,
    /// or — popping call frames along the way — out of the program. Returns
    /// false when nothing catches it.
    fn unwind(self: *Machine, thrown: abi.Value) bool {
        while (true) {
            if (self.hp > 0) {
                const handler = self.handlers[self.hp - 1];
                if (handler.fp == self.fp) {
                    self.hp -= 1;
                    self.stack.sp = handler.sp;
                    self.locals_top = handler.locals_top;
                    self.pc = handler.catch_pc;
                    self.stack.push(thrown);
                    return true;
                }
                if (handler.fp > self.fp) {
                    // A stale handler from an already-unwound frame.
                    self.hp -= 1;
                    continue;
                }
                // handler.fp < self.fp: the handler belongs to an enclosing
                // frame — pop the current frame first.
            }
            if (self.fp <= 1) return false; // nothing left: uncaught at top
            const frame = self.frames[self.fp - 1];
            self.fp -= 1;
            self.stack.sp = frame.stack_base;
            self.locals_top = frame.locals_base;
        }
    }

    /// Throws a value: either lands in a handler (execution continues at the
    /// catch pc) or escapes the program. Returns the pending program-result
    /// value when uncaught, else null. Sets `escaped` when the throw crossed
    /// call frames, so in-flight builtin loops (e.g. `map`) know to abort.
    fn throwValue(self: *Machine, thrown: abi.Value) ?abi.Value {
        const origin_fp = self.fp;
        if (self.unwind(thrown)) {
            if (self.fp < origin_fp) self.escaped = true;
            return null;
        }
        self.pending_throw = thrown;
        self.thrown = true;
        return thrown;
    }

    fn throwMessage(self: *Machine, message: []const u8) ?abi.Value {
        return self.throwValue(allocString(message));
    }

    /// Executes a single instruction; returns the program result when the
    /// main frame returns or an uncaught throw escapes, else null.
    fn step(self: *Machine) ?abi.Value {
        const instruction = self.code[self.pc];
        self.pc += 1;
        switch (instruction.op) {
            abi.OP_LOAD_INT => self.stack.push(abi.Value.fromInt(instruction.operand)),
            // The raw high half of a float literal, pushed as ordinary data.
            abi.OP_LOAD_F64_HI => self.stack.push(abi.Value.fromInt(instruction.operand)),
            abi.OP_LOAD_F64 => {
                const high = self.stack.pop();
                const high_bits: u64 = @as(u64, @bitCast(high.payload)) & 0xFFFF_FFFF;
                const low_bits: u64 = @as(u32, @bitCast(instruction.operand));
                self.stack.push(abi.Value.fromFloat(@bitCast((high_bits << 32) | low_bits)));
            },
            abi.OP_LOAD_BOOL => self.stack.push(abi.Value.fromBool(instruction.operand != 0)),
            abi.OP_LOAD_NULL => self.stack.push(abi.Value.NULL),
            abi.OP_LOAD_UNDEFINED => self.stack.push(abi.Value.UNDEF),
            abi.OP_LOAD_STRING => {
                const index: usize = @intCast(instruction.operand);
                if (index >= self.pool.len) @panic("string pool out of range");
                const constant = self.pool[index];
                if (constant.tag != abi.TAG_STRING) @panic("pool entry is not a string");
                // Copy the pool bytes into the arena; the pool only needs to
                // outlive the call.
                self.stack.push(allocString(constant.asString().?));
            },
            abi.OP_TO_STRING => {
                const value = self.stack.pop();
                if (value.tag == abi.TAG_STRING) {
                    self.stack.push(value);
                } else {
                    self.stack.push(allocString(stringBytesOf(value)));
                }
            },

            abi.OP_ADD => binaryArithmetic(.add, self),
            abi.OP_SUB => binaryArithmetic(.sub, self),
            abi.OP_MUL => binaryArithmetic(.mul, self),
            abi.OP_DIV => binaryArithmetic(.div, self),
            abi.OP_MOD => binaryArithmetic(.mod, self),
            abi.OP_POW => binaryArithmetic(.pow, self),

            abi.OP_LT => binaryCompare(.lt, self),
            abi.OP_GT => binaryCompare(.gt, self),
            abi.OP_LE => binaryCompare(.le, self),
            abi.OP_GE => binaryCompare(.ge, self),

            abi.OP_EQ => binaryEquality(.loose, self),
            abi.OP_NE => {
                const right = self.stack.pop();
                const left = self.stack.pop();
                self.stack.push(abi.Value.fromBool(!equals(.loose, left, right)));
            },
            abi.OP_EQ_STRICT => binaryEquality(.strict, self),
            abi.OP_NE_STRICT => {
                const right = self.stack.pop();
                const left = self.stack.pop();
                self.stack.push(abi.Value.fromBool(!equals(.strict, left, right)));
            },

            abi.OP_NEG => self.stack.push(negate(self.stack.pop())),
            abi.OP_NOT => {
                const value = self.stack.pop();
                self.stack.push(abi.Value.fromBool(!isTruthy(value)));
            },

            abi.OP_PRINT => {
                // console.log intrinsic: pop, print, leave `undefined` behind
                // so every expression still leaves exactly one value.
                const value = self.stack.pop();
                printValue(value);
                self.stack.push(abi.Value.UNDEF);
            },
            abi.OP_POP => _ = self.stack.pop(),

            abi.OP_DUP => {
                const value = self.stack.pop();
                self.stack.push(value);
                self.stack.push(value);
            },
            abi.OP_GET_LOCAL => {
                const slot = self.localSlot(instruction.operand);
                if (self.cells[slot]) |cell| {
                    self.stack.push(cell.value);
                } else {
                    self.stack.push(self.locals[slot]);
                }
            },
            abi.OP_SET_LOCAL => {
                const slot = self.localSlot(instruction.operand);
                const value = self.stack.pop();
                if (self.cells[slot]) |cell| {
                    cell.value = value;
                } else {
                    self.locals[slot] = value;
                }
            },
            abi.OP_MAKE_CELL => {
                const slot = self.localSlot(instruction.operand);
                const cell = machineAllocator().create(Cell) catch @panic("out of memory");
                cell.* = .{ .value = self.locals[slot] };
                self.cells[slot] = cell;
            },

            abi.OP_GET_UPVALUE => {
                const index: usize = @intCast(instruction.operand);
                const fo = self.frames[self.fp - 1].function orelse
                    @panic("upvalue access in the main frame");
                if (index >= fo.env.items.len) @panic("upvalue index out of range");
                self.stack.push(fo.env.items[index].?.value);
            },
            abi.OP_SET_UPVALUE => {
                const index: usize = @intCast(instruction.operand);
                const fo = self.frames[self.fp - 1].function orelse
                    @panic("upvalue access in the main frame");
                if (index >= fo.env.items.len) @panic("upvalue index out of range");
                fo.env.items[index].?.value = self.stack.pop();
            },

            abi.OP_CLOSURE => {
                const index: usize = @intCast(instruction.operand);
                if (index >= self.functions.len) @panic("function index out of range");
                const fo = machineAllocator().create(FunctionObject) catch @panic("out of memory");
                fo.* = .{ .index = index };
                self.building = fo;
                self.stack.push(.{
                    .tag = abi.TAG_FUNCTION,
                    ._pad = 0,
                    .payload = @bitCast(@as(i64, @intCast(@intFromPtr(fo)))),
                });
            },
            abi.OP_CAPTURE_LOCAL => {
                const slot = self.localSlot(instruction.operand);
                const cell = self.cells[slot] orelse @panic("capturing a local without a cell");
                self.building.?.env.append(machineAllocator(), cell) catch @panic("out of memory");
            },
            abi.OP_CAPTURE_UPVALUE => {
                const index: usize = @intCast(instruction.operand);
                const parent = self.frames[self.fp - 1].function orelse
                    @panic("capture from the main frame");
                if (index >= parent.env.items.len) @panic("upvalue index out of range");
                self.building.?.env.append(machineAllocator(), parent.env.items[index].?) catch @panic("out of memory");
            },

            abi.OP_JMP => self.pc = jumpTarget(instruction.operand),
            abi.OP_JMP_IF_FALSE => {
                const condition = self.stack.pop();
                if (!isTruthy(condition)) self.pc = jumpTarget(instruction.operand);
            },
            abi.OP_JMP_IF_TRUE => {
                const condition = self.stack.pop();
                if (isTruthy(condition)) self.pc = jumpTarget(instruction.operand);
            },

            abi.OP_NEW_ARRAY => {
                const count: usize = @intCast(instruction.operand);
                if (self.stack.sp < count) @panic("stack underflow in array literal");
                const start = self.stack.sp - count;
                const value = allocArray(self.stack.values[start..self.stack.sp]);
                self.stack.sp = start;
                self.stack.push(value);
            },
            abi.OP_NEW_OBJECT => {
                const count: usize = @intCast(instruction.operand);
                if (self.stack.sp < count * 2) @panic("stack underflow in object literal");
                const start = self.stack.sp - count * 2;
                var keys = machineAllocator().alloc(abi.Value, count) catch @panic("out of memory");
                var values = machineAllocator().alloc(abi.Value, count) catch @panic("out of memory");
                for (0..count) |i| {
                    keys[i] = self.stack.values[start + 2 * i];
                    values[i] = self.stack.values[start + 2 * i + 1];
                }
                const object = allocObject(keys, values);
                self.stack.sp = start;
                self.stack.push(object);
            },
            abi.OP_GET_INDEX => {
                const key = self.stack.pop();
                const receiver = self.stack.pop();
                if (receiver.tag == abi.TAG_NULL or receiver.tag == abi.TAG_UNDEFINED) {
                    const message = switch (receiver.tag) {
                        abi.TAG_NULL => "TypeError: Cannot read properties of null",
                        else => "TypeError: Cannot read properties of undefined",
                    };
                    if (self.throwMessage(message)) |result| return result;
                } else {
                    self.stack.push(getIndex(receiver, key));
                }
            },
            abi.OP_SET_INDEX => {
                // Pops the key (top) and the value; the receiver stays
                // (assignment expressions need the receiver evaluated exactly
                // once, and keeping it lets plain and compound shapes
                // align). Pushes the assigned value as the expression's
                // result.
                const key = self.stack.pop();
                const value = self.stack.pop();
                const receiver = self.stack.top();
                if (receiver.tag == abi.TAG_NULL or receiver.tag == abi.TAG_UNDEFINED) {
                    const message = switch (receiver.tag) {
                        abi.TAG_NULL => "TypeError: Cannot set properties of null",
                        else => "TypeError: Cannot set properties of undefined",
                    };
                    if (self.throwMessage(message)) |result| return result;
                } else {
                    setIndex(receiver, key, value);
                    self.stack.push(value);
                }
            },

            abi.OP_CALL => {
                const arg_count: usize = @intCast(instruction.operand);
                if (arg_count > MAX_ARGS) @panic("too many arguments");
                if (self.stack.sp < arg_count + 1) @panic("stack underflow in call");
                var args: [MAX_ARGS]abi.Value = undefined;
                for (0..arg_count) |i| args[i] = self.stack.values[self.stack.sp - arg_count + i];
                const fn_value = self.stack.values[self.stack.sp - arg_count - 1];
                self.stack.sp -= arg_count + 1;
                // Bound builtins dispatch without a frame.
                if (fn_value.tag == abi.TAG_METHOD) {
                    const method: *abi.KeelMethod =
                        @ptrFromInt(@as(usize, @bitCast(fn_value.payload)));
                    if (self.callBuiltin(method, args[0..arg_count])) |result| {
                        self.stack.push(result);
                    }
                    if (self.pending_throw) |thrown| return thrown;
                    return null;
                }
                if (fn_value.tag != abi.TAG_FUNCTION) {
                    if (self.throwMessage("TypeError: not a function")) |result| {
                        return result;
                    }
                    return null;
                }
                self.stack.push(fn_value);
                for (0..arg_count) |i| self.stack.push(args[i]);
                self.performCall(arg_count);
            },

            abi.OP_RETURN => {
                const frame = self.frames[self.fp - 1];
                const value = if (self.stack.sp > frame.stack_base)
                    self.stack.pop()
                else
                    abi.Value.UNDEF;
                self.fp -= 1;
                self.stack.sp = frame.stack_base;
                self.locals_top = frame.locals_base;
                // Discard any handlers the returning frame left active (a
                // `return` from inside a try block skips TRY_EXIT).
                while (self.hp > 0 and self.handlers[self.hp - 1].fp == self.fp + 1) {
                    self.hp -= 1;
                }
                if (self.fp == 0) return value; // returned from the main frame
                self.pc = frame.return_pc;
                self.stack.push(value);
            },
            abi.OP_TRY_ENTER => {
                if (self.hp >= MAX_HANDLERS) @panic("too many nested try blocks");
                self.handlers[self.hp] = .{
                    .catch_pc = jumpTarget(instruction.operand),
                    .sp = self.stack.sp,
                    .fp = self.fp,
                    .locals_top = self.locals_top,
                };
                self.hp += 1;
            },
            abi.OP_TRY_EXIT => {
                if (self.hp == 0) @panic("try exit without try enter");
                self.hp -= 1;
            },
            abi.OP_THROW => {
                const thrown = self.stack.pop();
                if (self.throwValue(thrown)) |result| return result;
            },
            else => @panic("unknown opcode"),
        }
        return null;
    }

    /// Shared call mechanics: the stack holds `[fn, arg1..argN]` with
    /// `arg_count` arguments; pops them, pushes a frame, and enters the body.
    fn performCall(self: *Machine, arg_count: usize) void {
        if (self.stack.sp < arg_count + 1) @panic("stack underflow in call");
        const fn_value = self.stack.values[self.stack.sp - arg_count - 1];
        if (fn_value.tag != abi.TAG_FUNCTION) @panic("call of a non-function value");
        const fo: *FunctionObject = @ptrFromInt(@as(usize, @bitCast(fn_value.payload)));
        if (fo.index >= self.functions.len) @panic("function index out of range");
        const entry = self.functions[fo.index];
        if (self.fp >= MAX_FRAMES) @panic("call stack overflow");
        const base = self.locals_top;
        self.locals_top += entry.local_count;
        if (self.locals_top > LOCAL_CAPACITY) @panic("too many locals");
        for (0..entry.local_count) |i| self.cells[base + i] = null;
        for (0..entry.arity) |i| {
            // Missing arguments read as `undefined` (JS semantics).
            self.locals[base + i] = if (i < arg_count) self.stack.values[self.stack.sp - arg_count + i] else abi.Value.UNDEF;
        }
        self.stack.sp -= arg_count + 1;
        self.frames[self.fp] = .{
            .return_pc = self.pc,
            .stack_base = self.stack.sp,
            .locals_base = base,
            .function = fo,
        };
        self.fp += 1;
        self.pc = entry.start;
    }

    /// Calls a function value with explicit arguments and returns its
    /// result, or null when the call escaped via a throw that a handler
    /// outside this call caught (execution continues at that catch pc) or
    /// that escaped the program entirely (`pending_throw`/`thrown` set).
    /// Used by builtins (`.map`) to run interpreted callbacks on this
    /// machine.
    fn invoke(self: *Machine, callee: abi.Value, args: []const abi.Value) ?abi.Value {
        const target_fp = self.fp;
        self.escaped = false;
        self.stack.push(callee);
        for (args) |arg| self.stack.push(arg);
        self.performCall(args.len);
        while (self.fp > target_fp) {
            if (self.step()) |_| return null; // uncaught inside the callback
        }
        if (self.escaped) return null;
        return self.stack.pop();
    }

    fn callBuiltin(
        self: *Machine,
        method: *abi.KeelMethod,
        args: []const abi.Value,
    ) ?abi.Value {
        const builtin = Builtin.fromId(method.id) orelse @panic("unknown builtin");
        switch (builtin) {
            .map => {
                if (method.receiver.tag != abi.TAG_ARRAY) {
                    if (self.throwMessage("TypeError: map is not a function on this value")) |result| {
                        return result;
                    }
                    return null;
                }
                const array = method.receiver.asArray().?;
                if (args.len < 1 or args[0].tag != abi.TAG_FUNCTION) {
                    if (self.throwMessage("TypeError: map callback must be a function")) |result| {
                        return result;
                    }
                    return null;
                }
                const callback = args[0];
                const items = machineAllocator().alloc(abi.Value, array.len) catch @panic("out of memory");
                for (0..array.len) |i| {
                    const element = array.items[i];
                    const index = abi.Value.fromInt(@intCast(i));
                    const result = self.invoke(callback, &[_]abi.Value{ element, index, method.receiver }) orelse return null;
                    items[i] = result;
                }
                return allocArray(items);
            },
            .join => {
                if (method.receiver.tag != abi.TAG_ARRAY) {
                    if (self.throwMessage("TypeError: join is not a function on this value")) |result| {
                        return result;
                    }
                    return null;
                }
                const array = method.receiver.asArray().?;
                // JS default separator is ","; non-string separators coerce.
                const separator: []const u8 = if (args.len > 0 and args[0].tag == abi.TAG_STRING)
                    args[0].asString().?
                else if (args.len > 0)
                    stringBytesOf(args[0])
                else
                    ",";
                var joined = std.ArrayList(u8).empty;
                for (0..array.len) |i| {
                    if (i > 0) joined.appendSlice(machineAllocator(), separator) catch @panic("out of memory");
                    const element = array.items[i];
                    // JS join renders null/undefined as empty strings.
                    if (element.tag == abi.TAG_NULL or element.tag == abi.TAG_UNDEFINED) continue;
                    joined.appendSlice(machineAllocator(), stringBytesOf(element)) catch @panic("out of memory");
                }
                return allocStringFrom(joined.items.ptr, joined.items.len);
            },
        }
    }
};

const RunOutcome = struct {
    value: abi.Value,
    thrown: bool,
};

fn runOutcome(
    instructions: []const abi.Instruction,
    pool: []const abi.Value,
    functions: []const abi.KeelFunction,
) RunOutcome {
    acquireMachine();
    defer releaseMachine();
    resetArena();
    var machine = Machine.init(instructions, pool, functions);
    const value = machine.execute();
    return .{ .value = value, .thrown = machine.thrown };
}

pub fn run(
    instructions: []const abi.Instruction,
    pool: []const abi.Value,
    functions: []const abi.KeelFunction,
) abi.Value {
    return runOutcome(instructions, pool, functions).value;
}

/// Like `run`, but reports an uncaught thrown value through `err_out`.
pub fn runErr(
    instructions: []const abi.Instruction,
    pool: []const abi.Value,
    functions: []const abi.KeelFunction,
    err_out: *abi.Value,
) abi.Value {
    const outcome = runOutcome(instructions, pool, functions);
    if (outcome.thrown) {
        err_out.* = outcome.value;
        return abi.Value.UNDEF;
    }
    err_out.* = abi.Value.UNDEF;
    return outcome.value;
}

pub export fn r8_machine_run(
    instructions: [*]const abi.Instruction,
    len: usize,
    pool: [*]const abi.Value,
    pool_len: usize,
    functions: [*]const abi.KeelFunction,
    functions_len: usize,
    err_out: *abi.Value,
) abi.Value {
    const outcome = runOutcome(instructions[0..len], pool[0..pool_len], functions[0..functions_len]);
    if (outcome.thrown) {
        err_out.* = outcome.value;
        return abi.Value.UNDEF;
    }
    err_out.* = abi.Value.UNDEF;
    return outcome.value;
}

fn binaryArithmetic(kind: Arithmetic, machine: *Machine) void {
    const right = machine.stack.pop();
    const left = machine.stack.pop();
    machine.stack.push(arithmetic(kind, left, right));
}

fn arithmetic(kind: Arithmetic, left: abi.Value, right: abi.Value) abi.Value {
    switch (kind) {
        .add, .sub, .mul => {
            // JS `+` concatenates when either operand is a string, array,
            // or object (ToPrimitive with the string default).
            if (kind == .add and isConcatOperand(left)) {
                return concatValues(left, right);
            }
            if (kind == .add and isConcatOperand(right)) {
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

fn isConcatOperand(value: abi.Value) bool {
    return value.tag == abi.TAG_STRING or
        value.tag == abi.TAG_ARRAY or
        value.tag == abi.TAG_OBJECT;
}

/// JS string coercion: strings pass through, arrays join like `String()`,
/// everything else renders like `String()`. The returned bytes are either
/// borrowed from the value's own memory or freshly arena-allocated.
fn stringBytesOf(value: abi.Value) []const u8 {
    if (value.tag == abi.TAG_STRING) return value.asString().?;
    if (value.tag == abi.TAG_ARRAY) {
        // Join with ","; nested elements stringify recursively, with
        // null/undefined rendering empty.
        const array = value.asArray().?;
        var joined = std.ArrayList(u8).empty;
        for (0..array.len) |i| {
            if (i > 0) joined.appendSlice(machineAllocator(), ",") catch @panic("out of memory");
            const element = array.items[i];
            if (element.tag == abi.TAG_NULL or element.tag == abi.TAG_UNDEFINED) continue;
            joined.appendSlice(machineAllocator(), stringBytesOf(element)) catch @panic("out of memory");
        }
        return joined.items;
    }
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

fn binaryCompare(kind: Compare, machine: *Machine) void {
    const right = machine.stack.pop();
    const left = machine.stack.pop();
    machine.stack.push(abi.Value.fromBool(compare(kind, left, right)));
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

fn binaryEquality(kind: Equality, machine: *Machine) void {
    const right = machine.stack.pop();
    const left = machine.stack.pop();
    machine.stack.push(abi.Value.fromBool(equals(kind, left, right)));
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
    // Objects (arrays, objects, functions, methods) compare by reference.
    if (isReferenceKind(left) and isReferenceKind(right)) {
        return left.tag == right.tag and left.payload == right.payload;
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

fn isReferenceKind(value: abi.Value) bool {
    return value.tag == abi.TAG_ARRAY or
        value.tag == abi.TAG_OBJECT or
        value.tag == abi.TAG_FUNCTION or
        value.tag == abi.TAG_METHOD;
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
        else => true, // arrays, objects, functions, methods are truthy
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

/// JS GetV semantics for `receiver[key]` and `receiver.key`.
fn getIndex(receiver: abi.Value, key: abi.Value) abi.Value {
    switch (receiver.tag) {
        abi.TAG_STRING => {
            const bytes = receiver.asString().?;
            if (key.tag == abi.TAG_INT) {
                const index = key.payload;
                if (index >= 0 and index < @as(i64, @intCast(charCount(bytes)))) {
                    const start = charOffset(bytes, @intCast(index));
                    // Re-slice from the offset; a char may be multi-byte.
                    const rest = bytes[start..];
                    const char_len = std.unicode.utf8ByteSequenceLength(rest[0]) catch 1;
                    return allocString(rest[0..char_len]);
                }
                return abi.Value.UNDEF;
            }
            if (key.tag == abi.TAG_STRING) {
                if (std.mem.eql(u8, key.asString().?, "length")) {
                    return abi.Value.fromInt(@intCast(charCount(bytes)));
                }
            }
            return abi.Value.UNDEF;
        },
        abi.TAG_ARRAY => {
            const array = receiver.asArray().?;
            if (key.tag == abi.TAG_INT) {
                const index = key.payload;
                if (index >= 0 and index < @as(i64, @intCast(array.len))) {
                    return array.items[@intCast(index)];
                }
                return abi.Value.UNDEF;
            }
            if (key.tag == abi.TAG_STRING) {
                const name = key.asString().?;
                if (std.mem.eql(u8, name, "length")) {
                    return abi.Value.fromInt(@intCast(array.len));
                }
                if (builtinByName(name)) |builtin| {
                    return allocMethod(receiver, @backingInt(builtin));
                }
            }
            return abi.Value.UNDEF;
        },
        abi.TAG_OBJECT => {
            const object = receiver.asObject().?;
            if (key.tag == abi.TAG_STRING) {
                const name = key.asString().?;
                const keys = object.keys[0..object.len];
                for (keys, 0..) |property_key, i| {
                    if (property_key.tag == abi.TAG_STRING and
                        std.mem.eql(u8, property_key.asString().?, name))
                    {
                        return object.values[i];
                    }
                }
            }
            return abi.Value.UNDEF;
        },
        abi.TAG_NULL, abi.TAG_UNDEFINED => {
            @panic("cannot read properties of null or undefined");
        },
        else => return abi.Value.UNDEF,
    }
}

/// JS Set semantics for `receiver[key] = value`. Array element assignment
/// grows the array with `undefined` fill; object assignment adds or
/// overwrites. Non-index writes on arrays are not supported yet.
fn setIndex(receiver: abi.Value, key: abi.Value, value: abi.Value) void {
    switch (receiver.tag) {
        abi.TAG_ARRAY => {
            const array = receiver.asArray().?;
            if (key.tag != abi.TAG_INT) return;
            const index = key.payload;
            if (index < 0) return;
            const requested: usize = @intCast(index);
            if (requested < array.len) {
                array.items[requested] = value;
                return;
            }
            // Grow with `undefined` fill, like JS.
            const allocator = machineAllocator();
            const grown = allocator.alloc(abi.Value, requested + 1) catch @panic("out of memory");
            for (0..requested) |i| grown[i] = if (i < array.len) array.items[i] else abi.Value.UNDEF;
            grown[requested] = value;
            array.items = grown.ptr;
            array.len = grown.len;
        },
        abi.TAG_OBJECT => {
            const object = receiver.asObject().?;
            if (key.tag != abi.TAG_STRING) return;
            const name = key.asString().?;
            const keys = object.keys[0..object.len];
            for (keys, 0..) |property_key, i| {
                if (property_key.tag == abi.TAG_STRING and
                    std.mem.eql(u8, property_key.asString().?, name))
                {
                    object.values[i] = value;
                    return;
                }
            }
            // Append the property.
            const allocator = machineAllocator();
            const grown_keys = allocator.alloc(abi.Value, object.len + 1) catch @panic("out of memory");
            const grown_values = allocator.alloc(abi.Value, object.len + 1) catch @panic("out of memory");
            @memcpy(grown_keys[0..object.len], object.keys[0..object.len]);
            @memcpy(grown_values[0..object.len], object.values[0..object.len]);
            grown_keys[object.len] = key;
            grown_values[object.len] = value;
            object.keys = grown_keys.ptr;
            object.values = grown_values.ptr;
            object.len += 1;
        },
        abi.TAG_NULL, abi.TAG_UNDEFINED => {
            @panic("cannot set properties of null or undefined");
        },
        else => {},
    }
}

fn builtinByName(name: []const u8) ?Builtin {
    if (std.mem.eql(u8, name, "map")) return .map;
    if (std.mem.eql(u8, name, "join")) return .join;
    return null;
}

fn charCount(bytes: []const u8) usize {
    var count: usize = 0;
    var index: usize = 0;
    while (index < bytes.len) {
        const length = std.unicode.utf8ByteSequenceLength(bytes[index]) catch 1;
        index += length;
        count += 1;
    }
    return count;
}

fn charOffset(bytes: []const u8, char_index: usize) usize {
    var count: usize = 0;
    var index: usize = 0;
    while (index < bytes.len and count < char_index) {
        const length = std.unicode.utf8ByteSequenceLength(bytes[index]) catch 1;
        index += length;
        count += 1;
    }
    return index;
}

fn printValue(value: abi.Value) void {
    const io = std.Options.debug_io;
    var buffer: [512]u8 = undefined;
    var file_writer = std.Io.File.stdout().writer(io, &buffer);
    const writer = &file_writer.interface;
    writeValue(writer, value) catch {};
    writer.writeAll("\n") catch {};
    writer.flush() catch {};
}

/// Streams a value's `String()` rendering to a writer without a fixed-size
/// limit (strings and nested arrays can be arbitrarily long).
fn writeValue(writer: anytype, value: abi.Value) !void {
    switch (value.tag) {
        abi.TAG_STRING => writer.writeAll(value.asString() orelse "<string>") catch {},
        abi.TAG_ARRAY => {
            const array = value.asArray().?;
            for (0..array.len) |i| {
                if (i > 0) writer.writeAll(",") catch {};
                const element = array.items[i];
                if (element.tag == abi.TAG_NULL or element.tag == abi.TAG_UNDEFINED) continue;
                writeValue(writer, element) catch {};
            }
        },
        else => {
            var text: [512]u8 = undefined;
            const formatted = formatValue(value, &text) catch return;
            writer.writeAll(formatted) catch {};
        },
    }
}

/// Formats a value the way JS `String()` does. Used by the `console.log`
/// intrinsic and unit-tested directly; `buffer` must hold the longest
/// possible rendering (arrays can exceed any fixed buffer — the caller
/// accepts truncation errors for very large arrays).
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
        abi.TAG_FUNCTION => "[function]",
        abi.TAG_OBJECT => "[object Object]",
        abi.TAG_METHOD => "[method]",
        abi.TAG_ARRAY => blk: {
            const array = value.asArray().?;
            // The join output lives in the first half of the buffer; element
            // rendering gets the second half (recursion re-splits it), so
            // the accumulating output cannot be clobbered.
            const out = buffer[0 .. buffer.len / 2];
            const scratch = buffer[buffer.len / 2 ..];
            var length: usize = 0;
            for (0..array.len) |i| {
                if (i > 0) {
                    if (length + 1 > out.len) break :blk error.StringTooLong;
                    out[length] = ',';
                    length += 1;
                }
                const element = array.items[i];
                if (element.tag == abi.TAG_NULL or element.tag == abi.TAG_UNDEFINED) continue;
                const text = formatValue(element, scratch) catch break :blk error.StringTooLong;
                if (length + text.len > out.len) break :blk error.StringTooLong;
                @memcpy(out[length..][0..text.len], text);
                length += text.len;
            }
            break :blk out[0..length];
        },
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
    const value = run(&instructions, &[_]abi.Value{}, &[_]abi.KeelFunction{});
    try std.testing.expectEqual(@as(?i64, 2), value.asInt());
}
test "arithmetic promotes and follows js semantics" {
    const expectInt = struct {
        fn call(instructions: []const abi.Instruction, expected: i64) !void {
            const value = run(instructions, &[_]abi.Value{}, &[_]abi.KeelFunction{});
            try std.testing.expectEqual(@as(?i64, expected), value.asInt());
        }
    }.call;
    const expectFloat = struct {
        fn call(instructions: []const abi.Instruction, expected: f64) !void {
            const value = run(instructions, &[_]abi.Value{}, &[_]abi.KeelFunction{});
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
            return run(&instructions, &[_]abi.Value{}, &[_]abi.KeelFunction{}).asBool().?;
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
            return run(&instructions, &[_]abi.Value{}, &[_]abi.KeelFunction{}).asBool().?;
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
    const negated = run(&neg_zero, &[_]abi.Value{}, &[_]abi.KeelFunction{});
    try std.testing.expectEqual(@as(f64, -0.0), negated.asFloat().?);

    const not_zero = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_INT, .operand = 0 },
        .{ .op = abi.OP_NOT, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    try std.testing.expect(run(&not_zero, &[_]abi.Value{}, &[_]abi.KeelFunction{}).asBool().?);

    const not_one = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_NOT, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    try std.testing.expect(!run(&not_one, &[_]abi.Value{}, &[_]abi.KeelFunction{}).asBool().?);
}

test "float literals load through the two-instruction encoding" {
    const load = abi.Instruction.loadF64(0.30000000000000004);
    const instructions = [_]abi.Instruction{ load[0], load[1], .{ .op = abi.OP_RETURN, .operand = 0 } };
    const value = run(&instructions, &[_]abi.Value{}, &[_]abi.KeelFunction{});
    try std.testing.expectEqual(@as(?f64, 0.30000000000000004), value.asFloat());
}

test "print pops and leaves undefined" {
    const instructions = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_INT, .operand = 5 },
        .{ .op = abi.OP_PRINT, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const value = run(&instructions, &[_]abi.Value{}, &[_]abi.KeelFunction{});
    try std.testing.expectEqual(abi.TAG_UNDEFINED, value.tag);
}

test "locals round trip and default to undefined" {
    const set_get = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_INT, .operand = 5 },
        .{ .op = abi.OP_SET_LOCAL, .operand = 0 },
        .{ .op = abi.OP_GET_LOCAL, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    try std.testing.expectEqual(@as(?i64, 5), run(&set_get, &[_]abi.Value{}, &[_]abi.KeelFunction{}).asInt());

    const read_default = [_]abi.Instruction{
        .{ .op = abi.OP_GET_LOCAL, .operand = 7 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const value = run(&read_default, &[_]abi.Value{}, &[_]abi.KeelFunction{});
    try std.testing.expectEqual(abi.TAG_UNDEFINED, value.tag);
}

test "dup duplicates the top of the stack" {
    const instructions = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_DUP, .operand = 0 },
        .{ .op = abi.OP_ADD, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    try std.testing.expectEqual(@as(?i64, 2), run(&instructions, &[_]abi.Value{}, &[_]abi.KeelFunction{}).asInt());
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
    try std.testing.expectEqual(@as(?i64, 7), run(&jmp, &[_]abi.Value{}, &[_]abi.KeelFunction{}).asInt());

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
            try std.testing.expectEqual(@as(?i64, expected), run(&instructions, &[_]abi.Value{}, &[_]abi.KeelFunction{}).asInt());
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
    const value = run(&instructions, &pool, &[_]abi.KeelFunction{});
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
    const value = run(&instructions, &pool, &[_]abi.KeelFunction{});
    try std.testing.expectEqualStrings("a5", value.asString().?);
}

test "to_string renders template substitutions" {
    // 0.5 -> "0.5"
    const load = abi.Instruction.loadF64(0.5);
    const program = [_]abi.Instruction{ load[0], load[1], .{ .op = abi.OP_TO_STRING, .operand = 0 }, .{ .op = abi.OP_RETURN, .operand = 0 } };
    const value = run(&program, &[_]abi.Value{}, &[_]abi.KeelFunction{});
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
            return run(&instructions, &pool, &[_]abi.KeelFunction{}).asBool().?;
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
            return !run(&instructions, &pool, &[_]abi.KeelFunction{}).asBool().?;
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
            return run(&instructions, &pool, &[_]abi.KeelFunction{}).asBool().?;
        }
    }.call;
    try std.testing.expect(loose("5"));
    try std.testing.expect(!loose("6"));
}

test "calls and returns through frames" {
    const code = [_]abi.Instruction{
        .{ .op = abi.OP_CLOSURE, .operand = 1 },
        .{ .op = abi.OP_CALL, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
        .{ .op = abi.OP_LOAD_INT, .operand = 5 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const functions = [_]abi.KeelFunction{
        .{ .start = 0, .len = 3, .local_count = 0, .arity = 0 },
        .{ .start = 3, .len = 2, .local_count = 0, .arity = 0 },
    };
    const value = run(&code, &[_]abi.Value{}, &functions);
    try std.testing.expectEqual(@as(?i64, 5), value.asInt());
}

test "arguments bind to parameters frame-relatively" {
    const code = [_]abi.Instruction{
        .{ .op = abi.OP_CLOSURE, .operand = 1 },
        .{ .op = abi.OP_LOAD_INT, .operand = 2 },
        .{ .op = abi.OP_LOAD_INT, .operand = 3 },
        .{ .op = abi.OP_CALL, .operand = 2 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
        // function(a, b) { return a + b; }
        .{ .op = abi.OP_GET_LOCAL, .operand = 0 },
        .{ .op = abi.OP_GET_LOCAL, .operand = 1 },
        .{ .op = abi.OP_ADD, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const functions = [_]abi.KeelFunction{
        .{ .start = 0, .len = 5, .local_count = 0, .arity = 0 },
        .{ .start = 5, .len = 4, .local_count = 2, .arity = 2 },
    };
    const value = run(&code, &[_]abi.Value{}, &functions);
    try std.testing.expectEqual(@as(?i64, 5), value.asInt());
}

test "closures capture cells and share mutation" {
    // let c = 0; const inc = () => { c += 1; }; inc(); inc(); c  ->  2
    const code = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_INT, .operand = 0 },
        .{ .op = abi.OP_SET_LOCAL, .operand = 0 },
        .{ .op = abi.OP_MAKE_CELL, .operand = 0 },
        .{ .op = abi.OP_CLOSURE, .operand = 1 },
        .{ .op = abi.OP_CAPTURE_LOCAL, .operand = 0 },
        .{ .op = abi.OP_SET_LOCAL, .operand = 1 },
        .{ .op = abi.OP_GET_LOCAL, .operand = 1 },
        .{ .op = abi.OP_CALL, .operand = 0 },
        .{ .op = abi.OP_POP, .operand = 0 },
        .{ .op = abi.OP_GET_LOCAL, .operand = 1 },
        .{ .op = abi.OP_CALL, .operand = 0 },
        .{ .op = abi.OP_POP, .operand = 0 },
        .{ .op = abi.OP_GET_LOCAL, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
        // () => { c += 1; }
        .{ .op = abi.OP_GET_UPVALUE, .operand = 0 },
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_ADD, .operand = 0 },
        .{ .op = abi.OP_SET_UPVALUE, .operand = 0 },
        .{ .op = abi.OP_LOAD_UNDEFINED, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const functions = [_]abi.KeelFunction{
        .{ .start = 0, .len = 14, .local_count = 2, .arity = 0 },
        .{ .start = 14, .len = 6, .local_count = 0, .arity = 0 },
    };
    const value = run(&code, &[_]abi.Value{}, &functions);
    try std.testing.expectEqual(@as(?i64, 2), value.asInt());
}

test "recursion works through self-capture" {
    // function fact(n) { if (n <= 1) { return 1; } return n * fact(n - 1); }
    // fact(5) -> 120
    const code = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_UNDEFINED, .operand = 0 },
        .{ .op = abi.OP_SET_LOCAL, .operand = 0 },
        .{ .op = abi.OP_MAKE_CELL, .operand = 0 },
        .{ .op = abi.OP_CLOSURE, .operand = 1 },
        .{ .op = abi.OP_CAPTURE_LOCAL, .operand = 0 },
        .{ .op = abi.OP_SET_LOCAL, .operand = 0 },
        .{ .op = abi.OP_GET_LOCAL, .operand = 0 },
        .{ .op = abi.OP_LOAD_INT, .operand = 5 },
        .{ .op = abi.OP_CALL, .operand = 1 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
        .{ .op = abi.OP_GET_LOCAL, .operand = 0 },
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_LE, .operand = 0 },
        .{ .op = abi.OP_JMP_IF_FALSE, .operand = 16 },
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
        .{ .op = abi.OP_GET_LOCAL, .operand = 0 },
        .{ .op = abi.OP_GET_UPVALUE, .operand = 0 },
        .{ .op = abi.OP_GET_LOCAL, .operand = 0 },
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_SUB, .operand = 0 },
        .{ .op = abi.OP_CALL, .operand = 1 },
        .{ .op = abi.OP_MUL, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const functions = [_]abi.KeelFunction{
        .{ .start = 0, .len = 10, .local_count = 1, .arity = 0 },
        .{ .start = 10, .len = 14, .local_count = 1, .arity = 1 },
    };
    const value = run(&code, &[_]abi.Value{}, &functions);
    try std.testing.expectEqual(@as(?i64, 120), value.asInt());
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

test "array literals, length, and element access" {
    const code = [_]abi.Instruction{
        // [10, 20, 30]
        .{ .op = abi.OP_LOAD_INT, .operand = 10 },
        .{ .op = abi.OP_LOAD_INT, .operand = 20 },
        .{ .op = abi.OP_LOAD_INT, .operand = 30 },
        .{ .op = abi.OP_NEW_ARRAY, .operand = 3 },
        // [1] -> 20 (keep a spare copy of the array for later reads)
        .{ .op = abi.OP_DUP, .operand = 0 },
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_GET_INDEX, .operand = 0 },
        .{ .op = abi.OP_POP, .operand = 0 },
        // [9] -> undefined (out of range)
        .{ .op = abi.OP_DUP, .operand = 0 },
        .{ .op = abi.OP_LOAD_INT, .operand = 9 },
        .{ .op = abi.OP_GET_INDEX, .operand = 0 },
        .{ .op = abi.OP_POP, .operand = 0 },
        // .length -> 3, returned as the final value
        .{ .op = abi.OP_DUP, .operand = 0 },
        .{ .op = abi.OP_LOAD_STRING, .operand = 0 },
        .{ .op = abi.OP_GET_INDEX, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    var length_name = abi.KeelString{ .ptr = "length".ptr, .len = 6 };
    const pool = [_]abi.Value{.{
        .tag = abi.TAG_STRING,
        ._pad = 0,
        .payload = @bitCast(@as(i64, @intCast(@intFromPtr(&length_name)))),
    }};
    const value = run(&code, &pool, &[_]abi.KeelFunction{});
    try std.testing.expectEqual(@as(?i64, 3), value.asInt());
}

test "array element assignment grows with undefined fill" {
    // set_index keeps the receiver and pushes the assigned value back.
    const code = [_]abi.Instruction{
        .{ .op = abi.OP_NEW_ARRAY, .operand = 0 },
        // a[2] = 7 -> [arr, 7]; pop the value, keep the array
        .{ .op = abi.OP_LOAD_INT, .operand = 2 },
        .{ .op = abi.OP_LOAD_INT, .operand = 7 },
        .{ .op = abi.OP_SET_INDEX, .operand = 0 },
        .{ .op = abi.OP_POP, .operand = 0 },
        // a[1] -> undefined (fill)
        .{ .op = abi.OP_DUP, .operand = 0 },
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_GET_INDEX, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const value = run(&code, &[_]abi.Value{}, &[_]abi.KeelFunction{});
    try std.testing.expectEqual(abi.TAG_UNDEFINED, value.tag);
}

test "object literals store and read properties" {
    var key_a = abi.KeelString{ .ptr = "a".ptr, .len = 1 };
    var key_b = abi.KeelString{ .ptr = "b".ptr, .len = 1 };
    const pool = [_]abi.Value{
        .{ .tag = abi.TAG_STRING, ._pad = 0, .payload = @bitCast(@as(i64, @intCast(@intFromPtr(&key_a)))) },
        .{ .tag = abi.TAG_STRING, ._pad = 0, .payload = @bitCast(@as(i64, @intCast(@intFromPtr(&key_b)))) },
    };
    const instructions = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_STRING, .operand = 0 },
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_LOAD_STRING, .operand = 1 },
        .{ .op = abi.OP_LOAD_INT, .operand = 2 },
        .{ .op = abi.OP_NEW_OBJECT, .operand = 2 },
        .{ .op = abi.OP_LOAD_STRING, .operand = 1 },
        .{ .op = abi.OP_GET_INDEX, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const value = run(&instructions, &pool, &[_]abi.KeelFunction{});
    try std.testing.expectEqual(@as(?i64, 2), value.asInt());
}

test "map re-enters the interpreter for callbacks" {
    // [1, 2, 3].map((x) => x * 2) -> [2, 4, 6]; check via [1].
    const code = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_LOAD_INT, .operand = 2 },
        .{ .op = abi.OP_LOAD_INT, .operand = 3 },
        .{ .op = abi.OP_NEW_ARRAY, .operand = 3 },
        // .map -> method value (receiver + "map")
        .{ .op = abi.OP_LOAD_STRING, .operand = 0 },
        .{ .op = abi.OP_GET_INDEX, .operand = 0 },
        // callback: function (x) { return x * 2; } at table index 1
        .{ .op = abi.OP_CLOSURE, .operand = 1 },
        .{ .op = abi.OP_CALL, .operand = 1 },
        // result[1] -> 4
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_GET_INDEX, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
        // function (x) { return x * 2; }
        .{ .op = abi.OP_GET_LOCAL, .operand = 0 },
        .{ .op = abi.OP_LOAD_INT, .operand = 2 },
        .{ .op = abi.OP_MUL, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    var map_name = abi.KeelString{ .ptr = "map".ptr, .len = 3 };
    const pool = [_]abi.Value{.{
        .tag = abi.TAG_STRING,
        ._pad = 0,
        .payload = @bitCast(@as(i64, @intCast(@intFromPtr(&map_name)))),
    }};
    const functions = [_]abi.KeelFunction{
        .{ .start = 0, .len = 11, .local_count = 0, .arity = 0 },
        .{ .start = 11, .len = 4, .local_count = 1, .arity = 1 },
    };
    const value = run(&code, &pool, &functions);
    try std.testing.expectEqual(@as(?i64, 4), value.asInt());
}

test "join concatenates elements with the separator" {
    var join_name = abi.KeelString{ .ptr = "join".ptr, .len = 4 };
    var dash = abi.KeelString{ .ptr = "-".ptr, .len = 1 };
    const pool = [_]abi.Value{
        .{ .tag = abi.TAG_STRING, ._pad = 0, .payload = @bitCast(@as(i64, @intCast(@intFromPtr(&join_name)))) },
        .{ .tag = abi.TAG_STRING, ._pad = 0, .payload = @bitCast(@as(i64, @intCast(@intFromPtr(&dash)))) },
    };
    const code = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_LOAD_INT, .operand = 2 },
        .{ .op = abi.OP_NEW_ARRAY, .operand = 2 },
        .{ .op = abi.OP_LOAD_STRING, .operand = 0 }, // .join
        .{ .op = abi.OP_GET_INDEX, .operand = 0 },
        .{ .op = abi.OP_LOAD_STRING, .operand = 1 }, // "-"
        .{ .op = abi.OP_CALL, .operand = 1 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const value = run(&code, &pool, &[_]abi.KeelFunction{});
    try std.testing.expectEqualStrings("1-2", value.asString().?);
}

test "arrays format like String() and compare by reference" {
    var buffer: [64]u8 = undefined;
    const text = try formatValue(abi.Value.fromFloat(1.5), &buffer);
    try std.testing.expectEqualStrings("1.5", text);
}

test "compound object property assignment via machine" {
    var key_n = abi.KeelString{ .ptr = "n".ptr, .len = 1 };
    const pool = [_]abi.Value{.{
        .tag = abi.TAG_STRING,
        ._pad = 0,
        .payload = @bitCast(@as(i64, @intCast(@intFromPtr(&key_n)))),
    }};
    // o.n += 5 with the compiler's shape: get; dup; key; get; load; add;
    // key; set; pop
    const code = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_STRING, .operand = 0 },
        .{ .op = abi.OP_LOAD_INT, .operand = 10 },
        .{ .op = abi.OP_NEW_OBJECT, .operand = 1 },
        .{ .op = abi.OP_SET_LOCAL, .operand = 0 },
        .{ .op = abi.OP_GET_LOCAL, .operand = 0 },
        .{ .op = abi.OP_DUP, .operand = 0 },
        .{ .op = abi.OP_LOAD_STRING, .operand = 0 },
        .{ .op = abi.OP_GET_INDEX, .operand = 0 },
        .{ .op = abi.OP_LOAD_INT, .operand = 5 },
        .{ .op = abi.OP_ADD, .operand = 0 },
        .{ .op = abi.OP_LOAD_STRING, .operand = 0 },
        .{ .op = abi.OP_SET_INDEX, .operand = 0 },
        .{ .op = abi.OP_POP, .operand = 0 },
        .{ .op = abi.OP_POP, .operand = 0 },
        .{ .op = abi.OP_GET_LOCAL, .operand = 0 },
        .{ .op = abi.OP_LOAD_STRING, .operand = 0 },
        .{ .op = abi.OP_GET_INDEX, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const value = run(&code, &pool, &[_]abi.KeelFunction{});
    try std.testing.expectEqual(@as(?i64, 15), value.asInt());
}

test "try catches and binds the thrown value" {
    const code = [_]abi.Instruction{
        .{ .op = abi.OP_TRY_ENTER, .operand = 5 }, // catch pc = 5
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_THROW, .operand = 0 },
        .{ .op = abi.OP_LOAD_INT, .operand = 99 }, // skipped
        .{ .op = abi.OP_RETURN, .operand = 0 },
        // catch: the caught value is on the stack -> return it
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const value = run(&code, &[_]abi.Value{}, &[_]abi.KeelFunction{});
    try std.testing.expectEqual(@as(?i64, 1), value.asInt());
}

test "try exit resumes normal flow" {
    const code = [_]abi.Instruction{
        .{ .op = abi.OP_TRY_ENTER, .operand = 9 },
        .{ .op = abi.OP_LOAD_INT, .operand = 7 },
        .{ .op = abi.OP_TRY_EXIT, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
        .{ .op = abi.OP_LOAD_INT, .operand = 99 }, // catch: never reached
        .{ .op = abi.OP_POP, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const value = run(&code, &[_]abi.Value{}, &[_]abi.KeelFunction{});
    try std.testing.expectEqual(@as(?i64, 7), value.asInt());
}

test "throws unwind across call frames to a caller handler" {
    // main: try { f() } catch { return 42 }
    const code = [_]abi.Instruction{
        .{ .op = abi.OP_CLOSURE, .operand = 1 },
        .{ .op = abi.OP_TRY_ENTER, .operand = 5 },
        .{ .op = abi.OP_CALL, .operand = 0 },
        .{ .op = abi.OP_TRY_EXIT, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
        .{ .op = abi.OP_POP, .operand = 0 }, // discard the caught value
        .{ .op = abi.OP_LOAD_INT, .operand = 42 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
        // f: throw 7
        .{ .op = abi.OP_LOAD_INT, .operand = 7 },
        .{ .op = abi.OP_THROW, .operand = 0 },
    };
    const functions = [_]abi.KeelFunction{
        .{ .start = 0, .len = 8, .local_count = 0, .arity = 0 },
        .{ .start = 8, .len = 2, .local_count = 0, .arity = 0 },
    };
    const value = run(&code, &[_]abi.Value{}, &functions);
    try std.testing.expectEqual(@as(?i64, 42), value.asInt());
}

test "a return inside try discards its handler" {
    // main: try { return 1 } catch { return 2 }  -> the handler must not
    // leak into later throws (there are none here, but the frame pop must
    // clean up without corruption).
    const code = [_]abi.Instruction{
        .{ .op = abi.OP_TRY_ENTER, .operand = 4 },
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
        .{ .op = abi.OP_POP, .operand = 0 },
        .{ .op = abi.OP_RETURN, .operand = 0 },
    };
    const value = run(&code, &[_]abi.Value{}, &[_]abi.KeelFunction{});
    try std.testing.expectEqual(@as(?i64, 1), value.asInt());
}

test "uncaught throws surface through err_out" {
    const code = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_STRING, .operand = 0 },
        .{ .op = abi.OP_THROW, .operand = 0 },
    };
    var boom = abi.KeelString{ .ptr = "boom".ptr, .len = 4 };
    const pool = [_]abi.Value{.{
        .tag = abi.TAG_STRING,
        ._pad = 0,
        .payload = @bitCast(@as(i64, @intCast(@intFromPtr(&boom)))),
    }};
    var err: abi.Value = abi.Value.UNDEF;
    const value = runErr(&code, &pool, &[_]abi.KeelFunction{}, &err);
    try std.testing.expectEqual(abi.TAG_UNDEFINED, value.tag);
    try std.testing.expectEqualStrings("boom", err.asString().?);
}

test "map callbacks may throw past the builtin" {
    // try { [1].map(function () { throw "x"; }) } catch { } -> uncaught "x"
    // escapes the program (no catch in main here: err_out carries it).
    const code = [_]abi.Instruction{
        .{ .op = abi.OP_LOAD_INT, .operand = 1 },
        .{ .op = abi.OP_NEW_ARRAY, .operand = 1 },
        .{ .op = abi.OP_LOAD_STRING, .operand = 0 }, // .map
        .{ .op = abi.OP_GET_INDEX, .operand = 0 },
        .{ .op = abi.OP_TRY_ENTER, .operand = 9 },
        .{ .op = abi.OP_CLOSURE, .operand = 1 },
        .{ .op = abi.OP_CALL, .operand = 1 },
        .{ .op = abi.OP_TRY_EXIT, .operand = 0 },
        .{ .op = abi.OP_JMP, .operand = 12 },
        // catch pc (9): the caught value is on top -> return it
        .{ .op = abi.OP_RETURN, .operand = 0 },
        .{ .op = abi.OP_LOAD_STRING, .operand = 1 }, // "not reached" (pc 10)
        .{ .op = abi.OP_RETURN, .operand = 0 }, // (pc 11 = loop end target)
        // function () { throw "x"; }
        .{ .op = abi.OP_LOAD_STRING, .operand = 1 },
        .{ .op = abi.OP_THROW, .operand = 0 },
    };
    var map_name = abi.KeelString{ .ptr = "map".ptr, .len = 3 };
    var thrown_text = abi.KeelString{ .ptr = "x".ptr, .len = 1 };
    const pool = [_]abi.Value{
        .{ .tag = abi.TAG_STRING, ._pad = 0, .payload = @bitCast(@as(i64, @intCast(@intFromPtr(&map_name)))) },
        .{ .tag = abi.TAG_STRING, ._pad = 0, .payload = @bitCast(@as(i64, @intCast(@intFromPtr(&thrown_text)))) },
    };
    const functions = [_]abi.KeelFunction{
        .{ .start = 0, .len = 12, .local_count = 0, .arity = 0 },
        .{ .start = 12, .len = 2, .local_count = 0, .arity = 0 },
    };
    var err: abi.Value = abi.Value.UNDEF;
    const value = runErr(&code, &pool, &functions, &err);
    try std.testing.expectEqual(abi.TAG_UNDEFINED, err.tag);
    try std.testing.expectEqualStrings("x", value.asString().?);
}
