# R8 — Engineering Status & Roadmap

*Last updated: 2026-11-17. This document records what the R8 engine does
today, how it is built, what has been verified, and exactly what remains on
the road to "V8 + Blink in one". The README carries the short version; this
file is the engineering detail.*

---

## 1. What R8 is

R8 is a from-scratch JavaScript engine with a **two-language architecture**:

- **Rust owns the language layer** — lexer, parser, AST, bytecode compiler,
  embedder API, and the conformance tooling.
- **Zig owns the machine** — a stack interpreter over the Keel bytecode,
  linked as a static library (`libr8_machine.a`) that Cargo builds via
  `crates/r8-runtime/build.rs` (Zig 0.17).

The two halves share **Keel**, a small versioned C ABI (`crates/r8-abi`):
tagged 16-byte values, 8-byte instructions, a string-constant pool, a
function table, and — critically — a set of **conformance probes** that let
the Rust side prove the hand-mirrored `zig/src/abi.zig` cannot silently
drift (every tag/opcode constant is walked across the FFI in tests and
`r8 selftest`).

The long-term goal (per the approved plan) is a full browser stack:
the engine above is the **V8 role**, and a clean-room port of Blink's
architecture (`corpus/blink/renderer/core` as reference) is the **Blink
role** — DOM, HTML, CSS, layout, paint — validated against
**web-platform-tests**. Corpora are cloned at pinned revisions
(`corpus-versions`); local runs may point at `../corpus`.

---

## 2. Workspace layout

```
crates/
  r8-abi        Keel: versioned C ABI (Value, Instruction, opcodes/tags,
                KeelString/KeelArray/KeelObject/KeelMethod/KeelFunction,
                conformance tables + FFI probes)
  r8-front      lexer, parser, AST, bytecode compiler (per-function buffers,
                constant-pool interning, scope/upvalue resolution, prescan)
  r8-runtime    parse → compile → machine crossing; error mapping
  r8-api        embedder facade
  r8-cli        the `r8` binary: eval / run --harness / selftest / version
  r8-core       future isolate/allocator/host-hook contract ("Trellis")
  r8-ir         future SSA IR for the optimizing tier
  r8-test262    Test262 conformance runner (classification, subprocess
                execution, buckets, reports, gates)
zig/src/
  abi.zig       hand-mirrored Keel ABI + conformance probe exports
  interp.zig    the Machine: stack interpreter, frames, cells, builtins
  heap.zig, baseline.zig, platform.zig   (future-tier stubs)
.github/workflows/
  ci.yml        fmt, clippy, cargo+zig tests, selftest, smoke fixtures,
                test262 curated-suite gates
  conformance.yml  nightly full Test262 run + scoreboard artifact
tests/smoke/    arith.js, strings.js, array.js (all passing)
scripts/fetch-corpus.sh, corpus-versions   pinned corpora
conformance/gates.txt                      per-suite minimum pass rates
```

Size today: **~12,000 lines** across the crates and the Zig machine
(r8-front is the largest at ~5,000 lines), 12 source/harness files, 2 CI
workflows, 16 commits.

---

## 3. Completed work

### Wave 1 — expressions + doubles (ABI v2)
- Full operator table with precedence climbing: arithmetic, comparison,
  equality, unary, `**` (right-assoc); float literals with exponents.
- `TAG_FLOAT`: doubles bit-packed into the unchanged 16-byte `Value`.
- Integers beyond `i32` compiled through the exact float path.
- `console.log` intrinsic (`OP_PRINT`) with JS-exact `Number::toString`
  rendering (shortest round-trip digits, ECMA-262 notation rules) shared
  by Rust `Display` and the Zig machine via identical test vectors.
- Block comments added later (the Test262 frontmatter caught the gap).

### Wave 2 — statements (ABI v3)
- `let`/`const`/`var` with multiple declarators, blocks, `if`/`else`
  (else-if chains), `while`, `for` (all clauses), empty statements.
- Assignment as an expression + compound forms; short-circuit `&&`/`||`
  (`OP_DUP` + conditional jumps so the deciding operand stays on the stack).
- Scope-aware compiler: every declaration gets a distinct local slot →
  block shadowing without runtime save/restore; jumps emitted with
  placeholders, patched after targets are known.
- Const writes and undeclared references were compile errors at this stage
  (later replaced by real sloppy-mode semantics — see Wave A2).

### Wave 3 — strings (ABI v4)
- `TAG_STRING` payload points at `KeelString {ptr, len}` owned by its
  creator (Rust pool or machine arena).
- String constants interned into a **constant pool** (`Compiled.pool`),
  passed to the machine, copied into the machine's arena on load — pool
  only needs to outlive the call.
- Lexer: escapes (`\n \t \r \b \f \v \0 \\ \' \" \xNN \u{...} \uNNNN`,
  identity escapes), unterminated-literal and raw-newline rejection.
- Template literals via a **lexer mode stack**: nested templates,
  brace counting inside substitutions; substitutions stringified via
  `OP_TO_STRING` so `${1}${2}` is `"12"`, never numeric addition.
- Machine-side **string arena**: runtime strings allocated per run,
  reset at the next run (documented lifetime; GC is a later phase).

### Wave 4 — functions & closures (ABI v5)
- `function` declarations and anonymous function expressions, arrow
  functions (paren-free single params, expression and block bodies),
  `return`, postfix call parsing with argument lists.
- **Per-body instruction buffers** concatenated into the flat stream in
  completion order with jump rebasing; the machine starts at
  `functions[0].start` (main is table entry 0).
- **JS-correct closures**: a shadowing-aware syntactic prescan decides
  which locals any nested function captures; those declarations emit
  `OP_MAKE_CELL`, moving the binding into an arena-allocated heap cell.
  Closures hold cell pointers → mutation is shared and cells outlive
  their frames (counters work; recursion works via self-capture of the
  declaration's own pre-created cell).
- Upvalue chains built transitively at reference resolution with a
  per-function upvalue map that also carries const-ness.
- Machine: call frames (return pc, operand-stack base, locals base) over a
  flat 512-slot locals array; `OP_CALL` binds arguments with JS semantics
  (missing → undefined, extras ignored).

### Wave 5 — arrays & objects (ABI v6)
- `TAG_ARRAY` / `TAG_OBJECT` (arena-owned storage), `TAG_METHOD`
  (bound builtin = receiver + method id).
- Unified property access: `a.b` compiles to `a["b"]` — one GetV opcode
  handling array elements, `.length`, builtin methods, object keys, and
  string indexing.
- Array literals (holes, trailing commas), object literals
  (identifier/string/computed keys + shorthand), property/index
  assignment (plain + compound) with the receiver evaluated exactly once
  (`set_index` pops key, pops value, keeps receiver, pushes the value
  back — plain and compound shapes align).
- `.map` re-enters the interpreter via `invoke` on the same machine state
  (callbacks run with `(element, index, array)`); `.join` stringifies per
  JS rules. The flat `run()` loop became a **`Machine` struct** for this.
- Element assignment grows arrays with `undefined` fill; null/undefined
  member access panics (TypeError stand-in until errors existed).

### Wave 6 — exceptions + Test262 infrastructure (ABI v7)
- `OP_TRY_ENTER`/`OP_TRY_EXIT`/`OP_THROW`: a handler stack (catch pc,
  stack depth, frame) with unwind that pops call frames until an
  enclosing handler matches; `return` inside `try` discards its handler.
- `r8_machine_run` gained `err_out` (the uncaught thrown value) →
  `EvalError::Uncaught` → CLI exit 1. Runtime member/call errors throw
  instead of panicking. Throws inside `.map` callbacks abort the builtin
  loop and land in the outer catch (an escape signal on the machine).
- Front end: `try`/`catch (e)`/`throw` with completion values; `finally`
  is a clean parse error for now.
- **Test262 runner** (`crates/r8-test262`): walks the pinned tc39 corpus,
  hand-rolled YAML frontmatter parser, static classification
  (module/async/generate/unshimmed includes → unsupported, excluded from
  rates), subprocess-per-test with poll-wait timeouts, buckets
  pass/fail/timeout/crash, JSON + Markdown scoreboard, gate enforcement
  (`conformance/gates.txt`).
- **CI**: nightly full run + scoreboard artifact (`conformance.yml`);
  curated language/* gates on every push (`ci.yml`).
- `corpus-versions` + `scripts/fetch-corpus.sh` (hermetic pinned clone;
  `R8_TEST262_ROOT` override).
- Lexer fix discovered by the first run: block comments.

### Wave A2 — language surface (ABI v8)
- `this` → per-run global object (`Frame.this_value`: global for normal
  calls, the fresh object for constructor calls).
- **`new` with real constructor semantics**: fresh object as `this`, JS
  return protocol (an object return wins, otherwise the constructed
  object).
- `typeof` (undeclared operands compile to `undefined + typeof`), ternary
  `?:` (with no-in mode for for-heads), full 32-bit bitwise/shift set
  (`& | ^ ~ << >> >>>` + compound assignments; ES ToInt32 reduction),
  `in`, `delete` (arrays get undefined holes), `do-while`, `switch`
  (test-chain + fall-through, break target), `break`/`continue` via a
  loop-context jump-collection stack, `for-in` over object keys and array
  indices.
- Hashbang comments; `\uXXXX`/`\u{hex}` escapes in identifiers.
- **Sloppy-mode global semantics**: undeclared reads compile to a global
  fallback (`OP_GLOBAL_GET`) that throws `ReferenceError: x is not
  defined` at runtime; undeclared assignments create the global
  (`OP_GLOBAL_SET`). This replaced the earlier compile-time error and
  unlocked the ReferenceError test families.

### Wave A3 — builtins (no ABI change)
- Global object populated at machine init: `String`/`Number`/`Boolean`
  coercions, `parseInt` (radix-aware, `0x` prefix), `parseFloat`
  (longest-prefix scan), `isNaN`/`isFinite`, `NaN`/`Infinity` values.
- **Math object**: abs/floor/ceil/round (JS half-up)/trunc/min/max/sqrt/
  pow/sign/log/exp, `PI`.
- **Error family** constructors (`Error`, `TypeError`, `RangeError`,
  `ReferenceError`, `SyntaxError`, `EvalError`) producing
  `{ name, message }` objects; both `new Error(msg)` and plain
  `Error(msg)` construct; throws carry real error objects.
- `Object.keys/values/entries`, `Array.isArray`, `Array(...)` constructor.
- **String methods** on string receivers: charAt, charCodeAt, indexOf,
  slice, substring, toUpperCase/toLowerCase, includes, startsWith,
  endsWith, trim, split, repeat, replace.
- **Array methods**: push, pop, shift, unshift, reverse, indexOf, slice,
  includes, concat.
- Constructor **statics** (`Number.isNaN`, `Object.keys`,
  `Array.isArray`, ...) dispatch from `GET_INDEX` on method values — no
  property storage on functions needed yet.

### Conformance infrastructure (Track 0 — complete)
Runner, shim harness, gates, CI, pinned corpora as described above. The
shim implements `assert.js`/`sta.js`/`compareArray.js`/`fnGlobalObject.js`
with only supported features; deviations are documented in the files
(`assert.throws` cannot verify error constructors yet; `assert` is an
object, not callable-with-properties; `propertyHelper.js` and friends are
unsupported → excluded from rates).

---

## 4. Current scoreboard (Test262)

Nightly job runs the full suite (22,205 files classified; 16,188 runnable
after static unsupported filtering). Curated language/* suites are also
enforced in CI. Measured state after the builtins wave:

| suite | pass rate | | suite | pass rate |
|---|---|---|---|---|
| language/keywords | 100% | | language/block-scope | 88% |
| language/future-reserved-words | 100% | | language/line-terminators | 78% |
| language/punctuators | 90% | | language/literals | 72% |
| language/types | 60% | | language/asi | 68% |
| language/comments | 68% | | language/white-space | 40% |
| language/identifiers | 50% | | language/reserved-words | 53% |
| language/directive-prologue | 50% | | language/expressions | 35% |
| language/statements | 26% | | language/statementList | 15% |
| language/source-text | 0% | | | |

Overall curated `language/*`: **5,699 / 16,188 runnable ≈ 35%**.
`conformance/gates.txt` pins these as minimums (CI fails on regression).

Known top failure causes (all roadmap items): `class` (~1,200 fails),
destructuring, `propertyHelper.js` machinery (unshimmed → unsupported),
generators, private-field syntax (`#x`), `eval`/`Object`/`Number`
built-in surface beyond the current set, strict-mode early errors.

Also verified: the three smoke fixtures (`arith.js`, `strings.js`,
`array.js`), `r8 selftest` (ABI v8-era checks), 38 Zig tests, ~90 Rust
tests across 8 suites, clippy `-D warnings`, rustfmt.

---

## 5. Key mechanisms (design notes for future work)

1. **Flat bytecode + function table.** Function bodies compile into
   per-body buffers that concatenate in completion order (innermost
   first, main last); the machine starts at `functions[0].start`.
   Absolute jumps are rebased at assembly. `KeelFunction {start, len,
   local_count, arity}`; entry 0 is main.

2. **Frames & locals.** A call frame owns a region of the flat 512-slot
   locals array plus an operand-stack base, so recursion nests naturally.
   `Frame {return_pc, stack_base, locals_base, function, this_value,
   new_target}` — `this_value` is the global object for normal calls,
   the fresh object for constructors.

3. **Closures via cells.** Captured bindings are moved into
   arena-allocated cells (`OP_MAKE_CELL`); access routes through the cell
   transparently; closures hold cell pointers, so mutation is shared and
   cells outlive frames. Upvalue chains are transitive
   (`OP_CAPTURE_LOCAL`/`OP_CAPTURE_UPVALUE`).

4. **Prototype chains (ABI v9, in progress).** `KeelObject` gains a
   `proto` link; property reads walk the chain (depth-32 cycle guard);
   writes stay own-property. Functions carry an own-properties object
   (`FunctionObject.properties`, created on demand) so `C.prototype`
   reads/writes work; `new C()` links the instance's proto to
   `C.prototype`; `instanceof` walks the chain (with builtin-aware
   semantics for the Error family / Array / Number / String).

5. **Builtin dispatch.** Method values (`TAG_METHOD` = receiver + id)
   route to implementations in `callBuiltin`/`callExtendedBuiltin`;
   shared names (indexOf/slice/concat/includes) dispatch on the receiver
   tag; constructor statics resolve through `builtinStatic`.

6. **Arena lifetime.** One arena per run, reset at the next run. Returned
   strings/objects/functions point into it and are valid until the next
   run — `r8-runtime` exposes `MACHINE_LOCK` and its tests serialize on
   it. The spinlock (`machine_lock`) serializes the machine itself;
   proper isolates are Phase 8.

7. **Error model.** Throws are values on the stack unwound via the
   handler stack; uncaught throws leave through `err_out` (Rust maps to
   `EvalError::Uncaught`); runtime errors (member access on null, calling
   non-functions) throw TypeError strings. Engine invariants (stack
   overflow, unknown opcodes) still panic → the runner's `crash` bucket.

---

## 6. Known deviations / limitations (documented in code)

- `assert.throws` cannot verify error constructor types yet
  (`instanceof` was just landing).
- `assert` is an object, not callable-with-properties.
- No strict-mode semantics (directives parse and are ignored).
- TDZ is a compile-time approximation; early errors (duplicate
  declarations, reserved-word assignments) are largely unimplemented —
  `$DONOTEVALUATE` tests fail honestly.
- `finally` blocks, labels, `for-of`, generators, destructuring,
  getters/setters, `instanceof` (mid-landing), `arguments`, rest/default
  params, computed-member `delete` nuances: unsupported or partial.
- Float formatting: exact shortest-round-trip, but `charCodeAt` reports
  code points (UTF-16 surrogate pairs approximate); `indexOf` on strings
  reports byte offsets for non-ASCII (documented gap).
- Error `.name` differentiation (TypeError vs Error) is generic.
- No `JSON`, no `RegExp`, no `Date`, no `Promise` (roadmap below).

---

## 7. Work in progress right now (uncommitted, ABI v9)

The prototype-chain work is **half-landed** on `master`'s working tree
(4 modified files; everything compiles, all Zig tests pass; one Rust
conformance-coverage test needs the new opcode added to its array):

- ✅ `crates/r8-abi`: ABI v9 (`KeelObject.proto`, `OP_INSTANCEOF`,
  conformance table + ctor, duplicate `KeelArray` fixed).
- ✅ `zig/src/abi.zig`: mirror complete (version 9, proto field,
  `OP_INSTANCEOF`, comptime length checks).
- ✅ `zig/src/interp.zig`: `allocObjectWithProto`, `findKeyDeep` (chain
  walk), `functionProperties`, prototype fallback in `getIndex`,
  function-property arms in `getIndex`/`setIndex`, chain-aware
  `hasProperty`, `OP_NEW` links instance proto from the constructor's
  `prototype` property, `OP_INSTANCEOF` arm + `isInstanceOf`
  (builtin-aware).
- ✅ `crates/r8-front`: `class`/`instanceof` keyword tokens added.
- ⬜ **Not started**: class parser + compiler arms (see §8.1),
  `instanceof` binary-operator wiring in the parser/compiler
  (`BinaryOp::InstanceOf` + opcode), prescan/walk arms for class bodies,
  tests.
- ⬜ The r8-abi conformance-coverage test array needs `OP_INSTANCEOF`
  appended (the one red test).

---

## 8. Remaining work — detailed

### 8.1 Finish prototype chains + classes (next session, in progress)

1. **instanceof wiring** — `BinaryOp::InstanceOf` variant, relational-
   level parse, compile to `OP_INSTANCEOF`; conformance-test array fix.
2. **Class declarations/expressions**:
   - Parse: `class Name { ... }` and `let C = class { ... }` / named
     class expressions; body items: `constructor(params) { }`, method
     definitions `name(params) { }`, `static name(...) { }`, string/computed
     method names; `get`/`set` → clean unsupported errors for now.
   - Compile: allocate a prototype object (`OP_NEW_OBJECT 0` into a
     temp slot); compile each method as a nested function; attach via
     `dup; key; closure; set_index` on the prototype; build the class
     value as a nested function whose body is the constructor; set its
     `prototype` property via the function-property path; bind the
     class name; `new C()` then links instance → prototype (machine
     already does this).
   - Walk arms for the closure prescan (methods are nested functions).
3. **Tests**: zig machine tests (proto read/write/fallback, instanceof),
   runtime tests (classes, methods, `this` in methods, instanceof against
   classes and builtin Error/Array), gate re-measure.
4. Expected Test262 movement: language/expressions class families
   (~1,200 files, many need more: getters, static blocks, private fields)
   — realistic target is a partial rate rise plus `instanceof` unlocks.

### 8.2 Runtime architecture (Phase 8 — GC, isolates, error objects)
- **GC**: mark-sweep over cells/objects/arrays/strings with roots = stack,
  frames, locals, global object, pending throw. Replaces the
  leak-per-run arena; required before long-running pages.
- **Isolates**: machine state per isolate (no global spinlock/arena);
  re-entrant host API (`r8_machine_eval/call`) — the `r8-core`
  isolate/allocator contract lands here.
- **Error objects as first-class**: `.stack`, subclassing, `Error.name`
  differentiation; runtime errors throw real Error objects.
- **Strict mode**: directive detection, strict function/eval semantics,
  early errors (this also converts `$DONOTEVALUATE` fails into passes).

### 8.3 Language wave A3 (built-ins depth)
- `Object.defineProperty/defineProperties/getOwnPropertyDescriptor/
  freeze/getPrototypeOf/setPrototypeOf` (descriptor machinery),
- `Array.prototype.forEach/filter/reduce/map-with-thisArg/sort`
  (sort needs a comparator callback path), `Function.prototype.apply/
  call/bind` (requires `this` threading through calls — frames already
  have `this_value`, so apply/call is an OP_CHANGE_THIS-style builtin),
- `String.prototype.charCodeAt` correctness for surrogate pairs,
- `JSON.stringify` (depth-capped), `JSON.parse` (recursive-descent
  subproject),
- getters/setters via object-literal `get`/`set` and defineProperty,
- destructuring (array/object patterns in declarations and assignment),
- `RegExp` (own engine — its own subproject),
- `Date`, `Promise` (needs the event loop — Track B dependency).

### 8.4 Track B — the Blink layer (clean-room from `corpus/blink`)
Each module is a crate, ported module-by-module from
`corpus/blink/renderer/core`, gated by WPT subdirectories:

| crate | ported from | gate |
|---|---|---|
| `r8-dom` | core/dom | WPT `dom/` subtest rate |
| `r8-html` | core/html/parser | html5lib corpus + WPT `html/syntax` |
| `r8-css` + `r8-style` | core/css | WPT cssom/selectors subsets |
| `r8-layout` | core/layout (LayoutNG) | layout reftests |
| `r8-paint` | core/paint | PNG snapshot reftests (CPU) |
| `r8-bindings` | renderer/bindings | WPT + runtime integration |
| `r8-shell` | the ContentRenderer role | WPT `html/` + demo page |

Key design points already decided:
- **Bindings** (the "v8+blink" glue): JS wrappers per DOM node, host
  objects exposed through the ABI (needs function-property storage —
  this session's `FunctionObject.properties` is the first step), host →
  machine calls via the re-entrant API, cross-heap GC coordination
  (DOM nodes refcounted, JS side traced, wrappers keep both alive).
- **Paint**: `PaintBackend` trait + display lists; **both** a CPU raster
  backend (deterministic PNG for CI snapshot diffs) and a GPU backend
  (wgpu, clean-room shaders) from launch, per product decision.
- **`r8-wpt`**: WPT manifest + native `testharness.js` execution (it runs
  inside our engine once DOM exists) → per-directory pass rates; nightly
  CI job.

### 8.5 Track C — product integration
`r8 browse page.html -o page.png` as the flagship; interactive windowing
later (Rune-hosted). Deterministic CPU rendering keeps CI honest.

### 8.6 Track D — tiers & perf (deliberately last)
Baseline tier (`zig/src/baseline.zig` stub exists), wire `r8-ir`, regex
engine work, interpreter dispatch tuning (threaded code / computed goto),
perf budgets. Conformance before speed.

---

## 9. Operating procedures

```sh
# build + test
cargo build --workspace && cargo test --workspace
cd zig && zig test src/root.zig && cd ..

# conformance
bash scripts/fetch-corpus.sh
cargo run -p r8-test262 -- --engine target/debug/r8 \
  --suite language/types --suite language/literals ... \
  --report conformance-results
cargo run -p r8-test262 -- --engine target/debug/r8 \
  --gate conformance/gates.txt --suite language/types ...

# fixture gates
cargo run -p r8-cli -- run tests/smoke/arith.js
cargo run -p r8-cli -- run tests/smoke/strings.js
cargo run -p r8-cli -- run tests/smoke/array.js
cargo run -p r8-cli -- selftest
```

Rules of the road: never lower a gate; raise ABI version for any
Value/Instruction/table change and mirror it in `zig/src/abi.zig` +
conformance tables + the coverage test in the same commit; the machine
panics only on invariants (they surface as the runner's `crash` bucket).

---

## 10. Decision log (deltas from the original plan)

- Corpora: cloned pinned upstream (not vendored, not the corpus checkout).
- Port policy: clean-room Rust from the Blink reference, no Servo crates.
- Paint: CPU raster + GPU backend both from launch (product decision).
- Test262: scoreboard + regression gates (not a percent-target-first
  approach).
- TDZ: compile-time approximation was replaced by runtime
  ReferenceError semantics for undeclared reads; declaration TDZ checks
  remain approximate until full early errors land.
- `instanceof`: builtin-aware partial semantics (Error family by name
  property, Array by tag) until user prototypes are fully live.
