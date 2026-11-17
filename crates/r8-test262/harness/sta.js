// R8 shim for Test262's `sta.js` harness (subset).
// `new Test262Error(message)` uses the engine's constructor-call machinery
// (OP_NEW): `this.message` lands on the fresh object, which is the result.
function Test262Error(message) {
  this.message = message || "";
}

function $DONOTEVALUATE() {
  throw "Test262: This statement should not be evaluated.";
}
