//! R8 embedder API. The vertical slice exposes evaluation and ABI identity.

pub use r8_abi::{ABI_VERSION, Value};
pub use r8_runtime::{EvalError, eval, eval_i64};

pub struct Engine;

impl Engine {
    pub fn new() -> Self {
        Self
    }

    pub fn eval(&self, source: &str) -> Result<i64, EvalError> {
        eval_i64(source)
    }
}

impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}
