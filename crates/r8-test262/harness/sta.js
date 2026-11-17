// R8 shim for Test262's `sta.js` harness (subset).
const Test262Error = {
  // Calling `Test262Error.thrower(message)` throws a distinguished string.
  // `new Test262Error(...)` is not supported by the engine yet, so tests
  // that use it fail honestly.
  thrower: function (message) {
    throw "Test262Error: " + (message || "");
  },
};

function $DONOTEVALUATE() {
  throw "Test262: This statement should not be evaluated.";
}
