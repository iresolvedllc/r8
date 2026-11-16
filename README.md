# R8

**R8** is a from-scratch JavaScript engine with a two-language architecture:
**Rust owns the language layer** (lexer, parser, AST, bytecode compiler) and
**Zig owns the machine** (the bytecode interpreter). The halves communicate
through **Keel**, a small versioned C ABI (`crates/r8-abi`), linked as a static
library built by Zig and consumed by Cargo.

R8 is a clean-room, V8-*inspired* design — it borrows V8's layered shape
(front end → bytecode → interpreter, with room for a baseline and optimizing
tier) without porting any of its code.

## Status

Phase 1: the expression language. R8 evaluates JavaScript **expressions** with
full operator precedence — arithmetic, comparison, equality, unary operators,
float literals, boolean/null/undefined literals, and a `console.log`
intrinsic. Numbers follow JS semantics (integer fast path, double promotion,
IEEE division) and print exactly like JS `Number::toString`.

Not yet: variables, control flow, functions, strings, objects/arrays
(see the roadmap).

## Usage

```sh
cargo run -p r8-cli -- eval "2 + 3 * 4"      # 14
cargo run -p r8-cli -- eval "0.1 + 0.2"      # 0.30000000000000004
cargo run -p r8-cli -- eval "1 / 0"          # Infinity
cargo run -p r8-cli -- run tests/smoke/arith.js
cargo run -p r8-cli -- selftest              # runtime ABI + eval smoke test
```

## Layout

```
crates/
  r8-abi       Keel: the versioned C ABI (Value, Instruction, opcodes/tags)
  r8-front     lexer, parser, AST, bytecode compiler
  r8-runtime   parse → compile → FFI crossing of the machine
  r8-api       thin embedder facade
  r8-cli       the `r8` binary (eval, run, selftest)
  r8-core      future isolate/allocator/host-hook contract ("Trellis")
  r8-ir        future SSA IR for the optimizing tier
zig/
  src/abi.zig  hand-mirrored Keel ABI + conformance probe exports
  src/interp.zig  the stack machine (the actual execution engine)
tests/smoke/  JS fixtures that double as milestone targets
```

Dependency chain: `r8-cli → r8-api → r8-runtime → r8-front → r8-abi`
(`r8-core` and `r8-ir` are future-facing and not yet wired in).

## Building

- Rust 1.99 (pinned in `rust-toolchain.toml`), via rustup
- Zig 0.17 on `$PATH` (`crates/r8-runtime/build.rs` shells out to
  `zig build-lib` and links `libr8_machine.a` statically)

```sh
cargo build --workspace
cargo test --workspace
zig test zig/src/root.zig
```

## Design notes

- **ABI versioning.** Keel has an `ABI_VERSION` that is checked at link time
  by `r8 selftest`. Every tag and opcode constant is additionally listed in a
  canonical conformance table in `crates/r8-abi` and mirrored, in the same
  order, by `zig/src/abi.zig`, which exports count/index probe functions.
  A test (and `r8 selftest`) walks both tables across the FFI boundary, so
  the hand-mirrored headers cannot silently drift.
- **Doubles.** `Value` is a 16-byte tagged struct (`tag: u32, payload: i64`).
  Doubles are bit-packed into the payload under `TAG_FLOAT`; integer literals
  that exceed `i32` are compiled through the float path (exact up to 2^53,
  matching JS double semantics).
- **`console.log` is an intrinsic.** It compiles to `OP_PRINT` and is
  implemented inside the machine. It will be replaced by a proper host
  function mechanism (via `r8-core` host hooks) once calls exist.

## Roadmap

| Phase | Milestone | Fixture gate |
|---|---|---|
| 1 | expression language + doubles | `tests/smoke/arith.js` |
| 2 | variables (`let`/`const`) + control flow (`if`/`while`/`for`) | — |
| 3 | strings, template literals, host built-ins | `tests/smoke/strings.js` |
| 4 | functions, calls, closures | — |
| 5 | arrays, objects, property access | `tests/smoke/array.js` |
| 6 | isolate/allocator wiring, GC, baseline tier, IR | — |

## License

Dual-licensed under MIT or Apache-2.0, at your option. See
`LICENSE-MIT` and `LICENSE-APACHE`.
