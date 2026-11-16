//! R8 runtime: compiles the front-end output and crosses the Keel ABI.

use r8_abi::Value;
use r8_front::{ParseError, compile, parse};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EvalError {
    #[error("parse error: {0}")]
    Parse(#[from] ParseError),
    #[error("result is not an integer")]
    NotAnInteger,
}

pub fn eval(source: &str) -> Result<Value, EvalError> {
    let expression = parse(source)?;
    let instructions = compile(&expression);
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
        assert!(eval_i64("1*1").is_err());
    }
}
