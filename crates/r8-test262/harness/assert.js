// R8 shim for Test262's `assert.js` harness — a subset implemented with only
// the language features the R8 engine supports today (no `new`, `typeof`,
// prototype machinery, or built-in globals).
//
// Known deviations from the real harness:
// - `assert.throws` verifies that the function throws, but cannot verify the
//   error CONSTRUCTOR yet (Error objects and `new` are not supported), so any
//   throw counts as matching.
// - `assert` is an object here; the real harness is also callable as
//   `assert(cond, msg)` (callable-with-properties is an engine TODO).
const NaN = 0 / 0;
const Infinity = 1 / 0;

// Error-constructor stand-ins: tests pass these to `assert.throws`. The shim
// cannot verify error types yet, so they only need to resolve.
const Error = "@@Error";
const TypeError = "@@TypeError";
const SyntaxError = "@@SyntaxError";
const RangeError = "@@RangeError";
const ReferenceError = "@@ReferenceError";
const EvalError = "@@EvalError";
const URIError = "@@URIError";
const AggregateError = "@@AggregateError";

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
