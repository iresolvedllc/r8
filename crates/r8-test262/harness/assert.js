// R8 shim for Test262's `assert.js` harness — a subset implemented with only
// the language features the R8 engine supports today (no `typeof`,
// prototype machinery, or callable-with-properties `assert`).
//
// Known deviations from the real harness:
// - `assert.throws` verifies that the function throws, but cannot verify the
//   error CONSTRUCTOR yet (instanceof is not implemented), so any throw
//   counts as matching.
// - `assert` is an object here; the real harness is also callable as
//   `assert(cond, msg)` (callable-with-properties is an engine TODO).
// - Error constructors (Error/TypeError/...) come from the engine's global
//   object; the shim does not shadow them.
const NaN = 0 / 0;
const Infinity = 1 / 0;

function sameValue(actual, expected) {
  if (actual !== actual && expected !== expected) {
    return true; // both NaN
  }
  return actual === expected;
}

function messageSuffix(message) {
  if (message === undefined) {
    return "";
  }
  return " " + message;
}

const assert = {};

assert.sameValue = function (actual, expected, message) {
  if (sameValue(actual, expected)) {
    return;
  }
  throw "Test262Error: SameValue failed" + messageSuffix(message);
};

assert.notSameValue = function (actual, unexpected, message) {
  if (!sameValue(actual, unexpected)) {
    return;
  }
  throw "Test262Error: NotSameValue failed" + messageSuffix(message);
};

assert.throws = function (expectedErrorConstructor, func, message) {
  try {
    func();
  } catch (thrown) {
    // The thrown value is intentionally not type-checked (see file header).
    return;
  }
  throw "Test262Error: did not throw" + messageSuffix(message);
};

assert.compareArray = function (actual, expected, message) {
  if (actual.length !== expected.length) {
    throw (
      "Test262Error: compareArray length mismatch" + messageSuffix(message)
    );
  }
  let index = 0;
  while (index < expected.length) {
    if (!sameValue(actual[index], expected[index])) {
      throw (
        "Test262Error: compareArray mismatch at " +
        index +
        messageSuffix(message)
      );
    }
    index += 1;
  }
};

// Shared with other shim files via the single program scope.
assert.sameValueHelper = sameValue;
