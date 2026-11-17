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

Wave A2 complete: the engine now runs `this` (per-run global object),
`typeof`, the ternary `?:`, the full 32-bit bitwise/shift set (`& | ^ ~
<< >> >>>` with compound assignments), `in`, `delete`, `do-while`, `switch`
(with fall-through and `break`), `break`/`continue`, `for-in` over object
keys, `new` constructor calls (JS result protocol: object return wins,
otherwise the fresh object), hashbang comments, `\uXXXX` identifier escapes,
block comments, and sloppy-mode global variable semantics (assignment
creates globals; reads of missing globals throw `ReferenceError`).

Not yet: `class`, generators, destructuring, getters, `instanceof`,
labels (see the roadmap and the conformance scoreboard).

## Usage

```sh
cargo run -p r8-cli -- eval "2 + 3 * 4"      # 14
cargo run -p r8-cli -- eval "0.1 + 0.2"      # 0.30000000000000004
cargo run -p r8-cli -- eval "1 / 0"          # Infinity
cargo run -p r8-cli -- eval "'a' + 5"        # a5
cargo run -p r8-cli -- eval "`x${1 + 1}y`"   # x2y
cargo run -p r8-cli -- eval "((f, x) => f(f(x)))((v) => v * v, 3)"  # 81
cargo run -p r8-cli -- eval "[1, 2, 3].map((v) => v * 2).join('-')" # 2-4-6
cargo run -p r8-cli -- eval "try { throw 'e'; } catch (e) { e }"    # e
cargo run -p r8-cli -- run tests/smoke/array.js
cargo run -p r8-cli -- selftest              # runtime ABI + eval smoke test
```

Not yet: `typeof`, `new`, classes, `break`/`continue`, `finally`, getters
(see the roadmap and the conformance scoreboard).

## Conformance

R8 tracks its conformance against **Test262** (JS) and, once the DOM layer
lands, **web-platform-tests**. The runner lives in `crates/r8-test262`:

```sh
bash scripts/fetch-corpus.sh                 # clone the pinned corpora
cargo run -p r8-test262 -- \
  --engine target/debug/r8 \
  --suite language/types --suite language/literals \
  --report conformance-results               # run + JSON/Markdown report
cargo run -p r8-test262 -- --gate conformance/gates.txt   # enforce rates
```

- Corpora are cloned at pinned revisions (`corpus-versions`); local runs can
  override with `R8_TEST262_ROOT` (e.g. an existing corpus checkout).
- Tests are classified from their frontmatter; `module`/`async`/`generate`
  tests and tests whose harness includes are not shimmed count as
  **unsupported** (excluded from rates).
- The shim harness (`crates/r8-test262/harness/`) reimplements the common
  Test262 helpers with only currently-supported engine features; known
  deviations are documented in the files.
- `conformance/gates.txt` pins minimum pass rates per suite prefix; CI
  (`conformance.yml`, nightly) runs the full suite and uploads the
  scoreboard, and `ci.yml` enforces the gates on every push/PR.

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
- **Functions compile per-body and concatenate.** Each function body gets
  its own instruction buffer; completed buffers concatenate into the flat
  stream (innermost first, main last) and the machine starts at
  `functions[0].start`. A call frame owns a region of the flat locals array
  plus its own operand-stack base, so recursion nests naturally.
- **Closures are correct via cells.** A syntactic, shadowing-aware prescan
  decides which locals any nested function captures; those bindings are
  moved into heap cells (`OP_MAKE_CELL`) that live in the machine's arena,
  so closures share mutation and outlive their frames. Upvalue chains are
  built transitively at reference resolution; recursion works because a
  function's own cell exists before its closure is created (self-capture).
- **Strings cross the FFI as pointers.** `TAG_STRING`'s payload points at a
  `KeelString {ptr, len}`. String *constants* are interned into a Rust-side
  pool passed to `r8_machine_run(code, len, pool, pool_len, functions,
  fn_len)`; the machine copies pool bytes into its own arena when a
  constant is loaded, so the pool only needs to outlive the call. Runtime
  strings (concatenations, template substitutions) are allocated in the
  machine's arena, which resets at the start of every run — returned string
  values are valid until the next run. Both are documented leaks until the
  GC (Phase 6). Because the machine holds global state, runs are serialized
  by a spinlock.
- **Jumps and locals.** `if`/`else`/`while`/`for` compile to absolute-index
  jumps (`OP_JMP`, `OP_JMP_IF_FALSE`, `OP_JMP_IF_TRUE`) whose targets are
  patched once known (and rebased when buffers concatenate); `&&`/`||` use
  `OP_DUP` plus conditional jumps so the deciding operand stays on the
  stack. Every declaration owns a distinct local slot, which makes block
  shadowing free at runtime; `let x = x` is rejected at compile time (a TDZ
  approximation until full TDZ checks land).

## Roadmap

| Phase | Milestone | Gate |
|---|---|---|
| 1 ✅ | expression language + doubles | `tests/smoke/arith.js` |
| 2 ✅ | variables, assignment, control flow, `&&`/`\|\|` | — |
| 3 ✅ | strings, template literals, string semantics | `tests/smoke/strings.js` |
| 4 ✅ | functions, calls, closures | — |
| 5 ✅ | arrays, objects, property access, `.map`/`.join` | `tests/smoke/array.js` |
| 6 ✅ | errors-as-values, `try`/`catch`/`throw` | Test262 gates (CI) |
| 7 ✅ | wave A2: `typeof`/`?:`/bitwise/`this`/`new`/`switch`/`for-in`/`break`/`continue` | Test262 gates ↑ (types 18→50) |
| 8 | language wave A3 + runtime architecture (GC, isolates) | Test262 rates ↑ |
| 9 | `r8-dom` + bindings (the Blink layer begins) | WPT `dom/` |
| 10 | `r8-html`, `r8-css`, `r8-style` | WPT `html/syntax`, cssom |
| 11 | `r8-layout` + `r8-paint` (CPU + GPU) | layout reftests, PNG snapshots |
| 12 | `r8-shell` event loop + `r8-wpt` runner | WPT `html/` rates |

## License

Dual-licensed under MIT or Apache-2.0, at your option. See
`LICENSE-MIT` and `LICENSE-APACHE`.
