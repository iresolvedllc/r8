//! R8 runtime: compiles the front-end output and crosses the Keel ABI.

use r8_abi::Value;
use r8_front::{CompileError, ParseError, compile, compile_program, parse, parse_program};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EvalError {
    #[error("parse error: {0}")]
    Parse(#[from] ParseError),
    #[error("compile error: {0}")]
    Compile(#[from] CompileError),
    #[error("result is not an integer")]
    NotAnInteger,
}

/// Evaluates a single expression and returns the resulting value.
pub fn eval(source: &str) -> Result<Value, EvalError> {
    let expression = parse(source)?;
    let instructions = compile(&expression)?;
    // SAFETY: the bytecode is a valid instruction stream for the machine.
    let value = unsafe { r8_abi::r8_machine_run(instructions.as_ptr(), instructions.len()) };
    Ok(value)
}

/// Evaluates a program (a sequence of statements) and returns the value of
/// the final expression statement, or `undefined` for programs that end
/// with any other statement.
pub fn eval_program(source: &str) -> Result<Value, EvalError> {
    let program = parse_program(source)?;
    let instructions = compile_program(&program)?;
    // SAFETY: the bytecode is a valid instruction stream for the machine.
    let value = unsafe { r8_abi::r8_machine_run(instructions.as_ptr(), instructions.len()) };
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
