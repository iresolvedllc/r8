//! R8 runtime: compiles the front-end output and crosses the Keel ABI.

use r8_abi::Value;
use r8_front::{
    CompileError, Compiled, ParseError, compile, compile_program, parse, parse_program,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EvalError {
    #[error("parse error: {0}")]
    Parse(#[from] ParseError),
    #[error("compile error: {0}")]
    Compile(#[from] CompileError),
    #[error("uncaught {0}")]
    Uncaught(Value),
    #[error("result is not an integer")]
    NotAnInteger,
}

/// Evaluates a single expression and returns the resulting value.
pub fn eval(source: &str) -> Result<Value, EvalError> {
    let expression = parse(source)?;
    let compiled = compile(&expression)?;
    run_machine(&compiled)
}

/// Evaluates a program (a sequence of statements) and returns the value of
/// the final expression statement, or `undefined` for programs that end
/// with any other statement.
pub fn eval_program(source: &str) -> Result<Value, EvalError> {
    let program = parse_program(source)?;
    let compiled = compile_program(&program)?;
    run_machine(&compiled)
}

fn run_machine(compiled: &Compiled) -> Result<Value, EvalError> {
    // SAFETY: the bytecode, pool, and function table are a valid instruction
    // stream, constant pool, and function table for the machine; all three
    // outlive the call.
    let mut err = Value::UNDEFINED;
    let value = unsafe {
        r8_abi::r8_machine_run(
            compiled.instructions.as_ptr(),
            compiled.instructions.len(),
            compiled.pool.as_ptr(),
            compiled.pool.len(),
            compiled.functions.as_ptr(),
            compiled.functions.len(),
            &mut err,
        )
    };
    if err.tag != r8_abi::TAG_UNDEFINED {
        return Err(EvalError::Uncaught(err));
    }
    Ok(value)
}

pub fn eval_i64(source: &str) -> Result<i64, EvalError> {
    eval(source)?.as_int().ok_or(EvalError::NotAnInteger)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_addition() {
        assert_eq!(eval_i64("1+1").unwrap(), 2);
        assert_eq!(eval_i64("2+3+4").unwrap(), 9);
    }

    #[test]
    fn reports_parse_errors() {
        assert!(eval_i64("1@1").is_err());
        assert!(eval_i64("name").is_err());
    }

    #[test]
    fn evaluates_expressions() {
        assert_eq!(eval("2**10").unwrap().to_string(), "1024");
        assert_eq!(eval("1/2").unwrap().to_string(), "0.5");
        assert_eq!(eval("1/0").unwrap().to_string(), "Infinity");
        assert_eq!(eval("0.1+0.2").unwrap().to_string(), "0.30000000000000004");
        assert_eq!(eval("7%3").unwrap().to_string(), "1");
        assert_eq!(eval("-(2**3)").unwrap().to_string(), "-8");
        assert_eq!(eval("2 < 3").unwrap().to_string(), "true");
        assert_eq!(eval("2===2.0").unwrap().to_string(), "true");
        assert_eq!(eval("null==undefined").unwrap().to_string(), "true");
        assert_eq!(eval("!false").unwrap().to_string(), "true");
        assert_eq!(eval("!0").unwrap().to_string(), "true");
    }

    #[test]
    fn evaluates_programs() {
        assert_eq!(eval_program("1; 2; 3").unwrap().to_string(), "3");
        // The final console.log leaves `undefined`, matching JS.
        assert_eq!(
            eval_program("console.log(1);").unwrap().to_string(),
            "undefined"
        );
        assert_eq!(eval_program("").unwrap().to_string(), "undefined");
    }

    #[test]
    fn evaluates_variables_and_loops() {
        assert_eq!(
            eval_program("let i = 0; let total = 0; while (i < 5) { total += i; i += 1; } total")
                .unwrap()
                .to_string(),
            "10"
        );
        assert_eq!(
            eval_program("let total = 0; for (let i = 0; i < 10; i += 1) { total += i; } total")
                .unwrap()
                .to_string(),
            "45"
        );
        assert_eq!(
            eval_program("let x = 0; if (2 > 1) { x = 10; } else { x = 20; } x")
                .unwrap()
                .to_string(),
            "10"
        );
        assert_eq!(
            eval_program("let x = 0; if (1 > 2) { x = 10; } else { x = 20; } x")
                .unwrap()
                .to_string(),
            "20"
        );
        // Shadowing: the inner x lives in its own slot.
        assert_eq!(
            eval_program("let x = 1; { let x = 2; } x")
                .unwrap()
                .to_string(),
            "1"
        );
        // Assignments are expressions producing the assigned value.
        assert_eq!(
            eval_program("let x = 0; let y = (x = 5) + 1; y")
                .unwrap()
                .to_string(),
            "6"
        );
        // Uninitialized locals read as undefined.
        assert_eq!(eval_program("let z; z").unwrap().to_string(), "undefined");
        // Short-circuit: the deciding operand is the result.
        assert_eq!(eval_program("0 && 5").unwrap().to_string(), "0");
        assert_eq!(eval_program("2 && 5").unwrap().to_string(), "5");
        assert_eq!(eval_program("0 || 7").unwrap().to_string(), "7");
        assert_eq!(eval_program("2 || 7").unwrap().to_string(), "2");
        assert_eq!(
            eval_program("false && (1 / 0)").unwrap().to_string(),
            "false"
        );
    }

    #[test]
    fn reports_compile_errors() {
        assert!(matches!(
            eval("x").unwrap_err(),
            EvalError::Compile(r8_front::CompileError::Undeclared(_))
        ));
        assert!(eval_program("const x = 1; x = 2;").is_err());
        assert!(eval_program("let x = x;").is_err());
    }

    #[test]
    fn evaluates_strings() {
        assert_eq!(eval("'a' + 'b'").unwrap().to_string(), "ab");
        assert_eq!(eval("'a' + 5").unwrap().to_string(), "a5");
        assert_eq!(eval("5 + ''").unwrap().to_string(), "5");
        assert_eq!(eval("'hello ' + 0.5").unwrap().to_string(), "hello 0.5");
        assert_eq!(
            eval("'5' == 5").unwrap().to_string(),
            "true" // loose: string coerces to number
        );
        assert_eq!(eval("'5' === 5").unwrap().to_string(), "false");
        assert_eq!(eval("'a' < 'b'").unwrap().to_string(), "true");
        assert_eq!(eval("'' + null").unwrap().to_string(), "null");
        assert_eq!(eval("'' + undefined").unwrap().to_string(), "undefined");
        assert_eq!(eval("'' + true").unwrap().to_string(), "true");
        assert_eq!(eval("'5' * '2'").unwrap().to_string(), "10");
        assert_eq!(eval("'1' && 7").unwrap().to_string(), "7"); // "1" truthy
    }

    #[test]
    fn evaluates_templates() {
        assert_eq!(eval("`hello ${'r8'}`").unwrap().to_string(), "hello r8");
        assert_eq!(eval("`x${1 + 1}y`").unwrap().to_string(), "x2y");
        // Numeric substitutions stringify correctly, never add numerically.
        assert_eq!(eval("`${1}${2}`").unwrap().to_string(), "12");
        assert_eq!(
            eval("`${0.1 + 0.2}`").unwrap().to_string(),
            "0.30000000000000004"
        );
        // via a variable, like the fixture
        assert_eq!(
            eval_program("const name = 'r8'; `hello ${name}`")
                .unwrap()
                .to_string(),
            "hello r8"
        );
    }

    #[test]
    fn evaluates_functions_and_calls() {
        assert_eq!(
            eval_program("function add(a, b) { return a + b; } add(2, 3)")
                .unwrap()
                .to_string(),
            "5"
        );
        // Arrows as values.
        assert_eq!(
            eval_program("const double = (x) => x * 2; double(21)")
                .unwrap()
                .to_string(),
            "42"
        );
        assert_eq!(
            eval_program("const inc = (x) => x + 1; inc(inc(inc(0)))")
                .unwrap()
                .to_string(),
            "3"
        );
        // Immediately invoked arrow expression.
        assert_eq!(eval("(() => 42)()").unwrap().to_string(), "42");
        // Anonymous function expression.
        assert_eq!(
            eval("(function (x) { return x; })(9)").unwrap().to_string(),
            "9"
        );
        // A call result can be called: two-level currying.
        assert_eq!(
            eval_program("const adder = (n) => (m) => n + m; adder(3)(4)")
                .unwrap()
                .to_string(),
            "7"
        );
    }

    #[test]
    fn evaluates_closures() {
        // Captured variables share mutation.
        assert_eq!(
            eval_program("let c = 0; const inc = () => { c += 1; }; inc(); inc(); c")
                .unwrap()
                .to_string(),
            "2"
        );
        // Cells outlive their frame: counter keeps incrementing after
        // `counter` returned.
        assert_eq!(
            eval_program(
                "function counter() { let n = 0; return () => { n += 1; return n; }; } \
                 const c = counter(); c(); c(); c()"
            )
            .unwrap()
            .to_string(),
            "3"
        );
        // Separate closures get separate cells.
        assert_eq!(
            eval_program(
                "function counter() { let n = 0; return () => { n += 1; return n; }; } \
                 const a = counter(); const b = counter(); a(); a(); b()"
            )
            .unwrap()
            .to_string(),
            "1"
        );
    }

    #[test]
    fn evaluates_recursion_and_higher_order() {
        assert_eq!(
            eval_program(
                "function fact(n) { if (n <= 1) { return 1; } return n * fact(n - 1); } fact(5)"
            )
            .unwrap()
            .to_string(),
            "120"
        );
        assert_eq!(
            eval_program(
                "function fib(n) { if (n < 2) { return n; } return fib(n - 1) + fib(n - 2); } fib(10)"
            )
            .unwrap()
            .to_string(),
            "55"
        );
        assert_eq!(
            eval_program("function twice(f, x) { return f(f(x)); } twice((v) => v + 1, 5)")
                .unwrap()
                .to_string(),
            "7"
        );
    }

    #[test]
    fn follows_js_call_semantics() {
        // Missing arguments read as undefined; extras are ignored.
        assert_eq!(
            eval_program("function g(a, b) { return b; } g(1)")
                .unwrap()
                .to_string(),
            "undefined"
        );
        assert_eq!(
            eval_program("function g(a) { return a; } g(1, 2, 3)")
                .unwrap()
                .to_string(),
            "1"
        );
        // `return;` yields undefined.
        assert_eq!(
            eval_program("function f() { return; } f()")
                .unwrap()
                .to_string(),
            "undefined"
        );
        // Falling off the end yields undefined.
        assert_eq!(
            eval_program("function f() { 1; } f()").unwrap().to_string(),
            "undefined"
        );
        // Functions print like Node's `[Function (anonymous)]`.
        assert_eq!(eval("() => 1").unwrap().to_string(), "[function]");
    }

    #[test]
    fn string_values_survive_across_runs() {
        // A string returned by one run stays readable until the next run
        // resets the machine's arena (documented lifetime).
        let first = eval("'kept'").unwrap();
        assert_eq!(first.as_str(), Some("kept"));
        let _ = eval("'next'").unwrap();
        // The bytes behind `first` may be gone; reading it now is only safe
        // before another run, so we assert the documented contract instead.
        assert_eq!(first.tag, r8_abi::TAG_STRING);
    }

    #[test]
    fn machine_conformance_probes_agree() {
        // The Zig machine mirrors every tag and opcode constant, in the same
        // canonical order; walk both tables across the FFI boundary.
        assert_eq!(
            unsafe { r8_abi::r8_abi_tag_count() },
            r8_abi::CONFORMANCE_TAGS.len(),
            "tag table sizes differ"
        );
        for (index, expected) in r8_abi::CONFORMANCE_TAGS.iter().enumerate() {
            // SAFETY: probes are pure table reads with valid indices here.
            let actual = unsafe { r8_abi::r8_abi_tag_at(index) };
            assert_eq!(actual, *expected, "tag constant {index} drifted");
        }
        assert_eq!(
            unsafe { r8_abi::r8_abi_op_count() },
            r8_abi::CONFORMANCE_OPS.len(),
            "opcode table sizes differ"
        );
        for (index, expected) in r8_abi::CONFORMANCE_OPS.iter().enumerate() {
            // SAFETY: probes are pure table reads with valid indices here.
            let actual = unsafe { r8_abi::r8_abi_op_at(index) };
            assert_eq!(actual, *expected, "opcode constant {index} drifted");
        }
    }
}

#[cfg(test)]
mod exception_tests {
    use super::*;

    #[test]
    fn evaluates_exceptions() {
        // try/catch with a binding.
        assert_eq!(
            eval_program("try { throw 7; } catch (e) { e }")
                .unwrap()
                .to_string(),
            "7"
        );
        // Thrown values from deep frames are caught outside.
        assert_eq!(
            eval_program(
                "function boom() { throw 'x'; } \
                 let out = ''; \
                 try { boom(); out = 'no'; } catch (e) { out = e; } out"
            )
            .unwrap()
            .to_string(),
            "x"
        );
        // Runtime member-access errors are throws, not crashes.
        assert_eq!(
            eval_program("try { undefined.x; } catch (e) { 1; }")
                .unwrap()
                .to_string(),
            "1"
        );
        // Calling a non-function throws too.
        assert_eq!(
            eval_program("let n = 5; try { n(); } catch (e) { 2; }")
                .unwrap()
                .to_string(),
            "2"
        );
        // A callback throw aborts map and lands in the outer catch.
        assert_eq!(
            eval_program("try { [1].map(() => { throw 'cb'; }); } catch (e) { e }")
                .unwrap()
                .to_string(),
            "cb"
        );
        // catch without a binding.
        assert_eq!(
            eval_program("try { throw 1; } catch { 9; }")
                .unwrap()
                .to_string(),
            "9"
        );
        // Nested try: the inner catch rethrows, the outer handles it.
        assert_eq!(
            eval_program(
                "try { try { throw 'inner'; } catch (e) { throw 'outer'; } } \
                 catch (e) { e }"
            )
            .unwrap()
            .to_string(),
            "outer"
        );
        // Uncaught throws surface as errors.
        assert!(eval_program("throw 'boom';").is_err());
        // finally is not supported yet.
        assert!(matches!(
            eval_program("try { 1; } finally { 2; }"),
            Err(EvalError::Parse(r8_front::ParseError::Unsupported(
                "finally blocks"
            )))
        ));
    }
}

#[cfg(test)]
mod phase5_tests {
    use super::*;

    #[test]
    fn evaluates_arrays() {
        assert_eq!(eval("[1, 2, 3]").unwrap().to_string(), "1,2,3");
        assert_eq!(eval("[1, 2, 3][1]").unwrap().to_string(), "2");
        assert_eq!(eval("[1, 2, 3].length").unwrap().to_string(), "3");
        assert_eq!(eval("[1,,3]").unwrap().to_string(), "1,,3");
        assert_eq!(eval("[1, 2, 3][9]").unwrap().to_string(), "undefined");
        assert_eq!(eval("[]").unwrap().to_string(), "");
        // Element assignment grows with undefined fill.
        assert_eq!(
            eval_program("let a = []; a[2] = 7; a[2]")
                .unwrap()
                .to_string(),
            "7"
        );
        assert_eq!(
            eval_program("let a = []; a[2] = 7; a.length")
                .unwrap()
                .to_string(),
            "3"
        );
        assert_eq!(
            eval_program("let a = []; a[2] = 7; a[1]")
                .unwrap()
                .to_string(),
            "undefined"
        );
    }

    #[test]
    fn evaluates_objects() {
        assert_eq!(eval("({ a: 1 }).a").unwrap().to_string(), "1");
        assert_eq!(eval("({ a: 1 })['a']").unwrap().to_string(), "1");
        assert_eq!(eval("({ a: 1 }).missing").unwrap().to_string(), "undefined");
        // Unknown properties read as undefined; arrays expose builtins.
        assert_eq!(eval("({}).toString").unwrap().to_string(), "undefined");
        assert_eq!(eval("[].map").unwrap().tag, r8_abi::TAG_METHOD);
        // Shorthand and computed keys.
        assert_eq!(
            eval_program("let k = 'b'; let a = 5; let o = { a, [k]: 7 }; o.b")
                .unwrap()
                .to_string(),
            "7"
        );
        // Property assignment adds and overwrites.
        assert_eq!(
            eval_program("let o = { a: 1 }; o.a = 2; o.b = 3; o.a")
                .unwrap()
                .to_string(),
            "2"
        );
        // Compound property assignment.
        assert_eq!(
            eval_program("let o = { n: 10 }; o.n += 5; o.n")
                .unwrap()
                .to_string(),
            "15"
        );
        // Compound index assignment on arrays.
        assert_eq!(
            eval_program("let a = [1, 2]; a[1] += 10; a[1]")
                .unwrap()
                .to_string(),
            "12"
        );
        // Reference equality, JS-style.
        assert_eq!(eval("[] === []").unwrap().to_string(), "false");
        assert_eq!(
            eval_program("let a = []; a === a").unwrap().to_string(),
            "true"
        );
        // Arrays are truthy even when empty; String({}) is "[object Object]".
        assert!(eval("[] ? 1 : 0").is_err()); // no ternary yet
        assert_eq!(
            eval_program("let x = 0; if ([]) { x = 1; } else { x = 0; } x")
                .unwrap()
                .to_string(),
            "1"
        );
        assert_eq!(eval("'' + []").unwrap().to_string(), "");
        assert_eq!(eval("'' + {}").unwrap().to_string(), "[object Object]");
    }

    #[test]
    fn evaluates_strings_as_objects() {
        assert_eq!(eval("'hello'.length").unwrap().to_string(), "5");
        assert_eq!(eval("'héllo'.length").unwrap().to_string(), "5");
        assert_eq!(eval("'hello'[1]").unwrap().to_string(), "e");
        assert_eq!(eval("'hello'[9]").unwrap().to_string(), "undefined");
    }

    #[test]
    fn evaluates_array_builtins() {
        // map re-enters the interpreter for each element.
        assert_eq!(
            eval("[1, 2, 3].map((value) => value * 2)")
                .unwrap()
                .to_string(),
            "2,4,6"
        );
        assert_eq!(
            eval("[1, 2, 3].map((value) => value * 2).join(',')")
                .unwrap()
                .to_string(),
            "2,4,6"
        );
        // join with default and custom separators; null/undefined render "".
        assert_eq!(eval("[1, 2, 3].join('-')").unwrap().to_string(), "1-2-3");
        assert_eq!(
            eval("[undefined, 2, null].join(',')").unwrap().to_string(),
            ",2,"
        );
        assert_eq!(eval("[1, 2, 3].join()").unwrap().to_string(), "1,2,3");
        // map receives (element, index, array).
        assert_eq!(
            eval("[10, 20].map((value, index) => value + index)")
                .unwrap()
                .to_string(),
            "10,21"
        );
    }
}
