//! R8 embedder API. Exposes evaluation (expressions and programs) and ABI
//! identity.

pub use r8_abi::{ABI_VERSION, Value};
pub use r8_runtime::{EvalError, eval, eval_i64, eval_program};

pub struct Engine;

impl Engine {
    pub fn new() -> Self {
        Self
    }

    /// Evaluates a single expression, returning the value in its JS type.
    pub fn eval(&self, source: &str) -> Result<Value, EvalError> {
        eval(source)
    }

    /// Evaluates a program of expression statements.
    pub fn eval_program(&self, source: &str) -> Result<Value, EvalError> {
        eval_program(source)
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}
