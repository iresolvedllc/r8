// R8 shim for Test262's `fnGlobalObject.js` harness (subset). The shimmed
// global object is an empty object, not the real global scope, so tests
// that expect `var` declarations to appear on it fail honestly.
const globalThis = {};

function fnGlobalObject() {
  return globalThis;
}
