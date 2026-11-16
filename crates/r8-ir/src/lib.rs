//! r8-ir: the clean SSA IR for the future optimizing tier.
//!
//! The baseline JIT consumes bytecode. This IR exists so the optimizing tier
//! can be designed without porting Turboshaft or Maglev.

use std::fmt::Write as _;

use thiserror::Error;

pub type ValueId = u32;
pub type BlockId = u32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Const(i64),
    Add(ValueId, ValueId),
    Return(ValueId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    pub result: Option<ValueId>,
    pub op: Op,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub id: BlockId,
    pub instructions: Vec<Instruction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IrError {
    #[error("expected a function header")]
    ExpectedFunction,
    #[error("expected a block header")]
    ExpectedBlock,
    #[error("unexpected line: {0}")]
    Unexpected(String),
    #[error("unclosed block")]
    UnclosedBlock,
}

impl Function {
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        writeln!(text, "fn {}() {{", self.name).ok();
        for block in &self.blocks {
            writeln!(text, "  block {} {{", block.id).ok();
            for instruction in &block.instructions {
                match instruction.op {
                    Op::Const(value) => {
                        writeln!(
                            text,
                            "    v{} = const {}",
                            instruction.result.unwrap_or_default(),
                            value
                        )
                        .ok();
                    }
                    Op::Add(left, right) => {
                        writeln!(
                            text,
                            "    v{} = add v{}, v{}",
                            instruction.result.unwrap_or_default(),
                            left,
                            right
                        )
                        .ok();
                    }
                    Op::Return(value) => {
                        writeln!(text, "    return v{value}").ok();
                    }
                }
            }
            writeln!(text, "  }}").ok();
        }
        writeln!(text, "}}").ok();
        text
    }

    pub fn from_text(text: &str) -> Result<Self, IrError> {
        let mut name = None;
        let mut blocks = Vec::new();
        let mut current_block: Option<Block> = None;

        for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
            if line.starts_with("fn ") {
                let rest = line.strip_prefix("fn ").ok_or(IrError::ExpectedFunction)?;
                let function_name = rest.split("()").next().ok_or(IrError::ExpectedFunction)?;
                name = Some(function_name.trim().to_string());
                continue;
            }
            if let Some(rest) = line.strip_prefix("block ") {
                let id = rest
                    .trim_end_matches('{')
                    .trim()
                    .parse::<BlockId>()
                    .map_err(|_| IrError::ExpectedBlock)?;
                if current_block.is_some() {
                    return Err(IrError::UnclosedBlock);
                }
                current_block = Some(Block {
                    id,
                    instructions: Vec::new(),
                });
                continue;
            }
            if line == "}" {
                if let Some(block) = current_block.take() {
                    blocks.push(block);
                }
                continue;
            }
            let Some(block) = current_block.as_mut() else {
                return Err(IrError::Unexpected(line.to_string()));
            };
            if let Some(rest) = line.strip_prefix('v') {
                let (id_text, operation) = rest
                    .split_once('=')
                    .ok_or_else(|| IrError::Unexpected(line.to_string()))?;
                let result = id_text
                    .trim()
                    .parse::<ValueId>()
                    .map_err(|_| IrError::Unexpected(line.to_string()))?;
                let operation = operation.trim();
                if let Some(value) = operation.strip_prefix("const ") {
                    let value = value
                        .parse::<i64>()
                        .map_err(|_| IrError::Unexpected(line.to_string()))?;
                    block.instructions.push(Instruction {
                        result: Some(result),
                        op: Op::Const(value),
                    });
                } else if let Some(operands) = operation.strip_prefix("add ") {
                    let (left, right) = operands
                        .split_once(',')
                        .ok_or_else(|| IrError::Unexpected(line.to_string()))?;
                    let left = left
                        .trim()
                        .trim_start_matches('v')
                        .parse::<ValueId>()
                        .map_err(|_| IrError::Unexpected(line.to_string()))?;
                    let right = right
                        .trim()
                        .trim_start_matches('v')
                        .parse::<ValueId>()
                        .map_err(|_| IrError::Unexpected(line.to_string()))?;
                    block.instructions.push(Instruction {
                        result: Some(result),
                        op: Op::Add(left, right),
                    });
                } else {
                    return Err(IrError::Unexpected(line.to_string()));
                }
            } else if let Some(value) = line.strip_prefix("return v") {
                let value = value
                    .parse::<ValueId>()
                    .map_err(|_| IrError::Unexpected(line.to_string()))?;
                block.instructions.push(Instruction {
                    result: None,
                    op: Op::Return(value),
                });
            } else {
                return Err(IrError::Unexpected(line.to_string()));
            }
        }

        if current_block.is_some() {
            return Err(IrError::UnclosedBlock);
        }
        let name = name.ok_or(IrError::ExpectedFunction)?;
        Ok(Self { name, blocks })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trip() {
        let function = Function {
            name: "add".to_string(),
            blocks: vec![Block {
                id: 0,
                instructions: vec![
                    Instruction {
                        result: Some(0),
                        op: Op::Const(1),
                    },
                    Instruction {
                        result: Some(1),
                        op: Op::Const(1),
                    },
                    Instruction {
                        result: Some(2),
                        op: Op::Add(0, 1),
                    },
                    Instruction {
                        result: None,
                        op: Op::Return(2),
                    },
                ],
            }],
        };
        let text = function.to_text();
        assert_eq!(Function::from_text(&text).unwrap(), function);
    }
}
