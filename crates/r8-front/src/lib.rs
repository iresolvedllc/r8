//! R8 front end: lexer, parser, AST, and bytecode compiler.
//!
//! The vertical slice handles integer literals and addition. The parser is
//! deliberately shaped like the real ES2015+ front end: tokens, precedence,
//! AST, then bytecode.

use r8_abi::Instruction;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Int(i64),
    Add(Box<Expr>, Box<Expr>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Int(i64),
    Plus,
    Semicolon,
    Eof,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("unexpected character '{0}'")]
    Unexpected(char),
    #[error("unexpected end of input")]
    UnexpectedEnd,
    #[error("expected an integer")]
    ExpectedInteger,
    #[error("trailing input")]
    TrailingInput,
}

pub fn tokenize(source: &str) -> Result<Vec<Token>, ParseError> {
    let characters: Vec<char> = source.chars().collect();
    let mut index = 0;
    let mut tokens = Vec::new();
    while index < characters.len() {
        let character = characters[index];
        match character {
            character if character.is_whitespace() => {
                index += 1;
            }
            '+' => {
                tokens.push(Token::Plus);
                index += 1;
            }
            ';' => {
                tokens.push(Token::Semicolon);
                index += 1;
            }
            character if character.is_ascii_digit() => {
                let mut value = 0i64;
                while index < characters.len() && characters[index].is_ascii_digit() {
                    value = value * 10 + (characters[index] as i64 - '0' as i64);
                    index += 1;
                }
                tokens.push(Token::Int(value));
            }
            other => return Err(ParseError::Unexpected(other)),
        }
    }
    tokens.push(Token::Eof);
    Ok(tokens)
}

pub fn parse(source: &str) -> Result<Expr, ParseError> {
    let tokens = tokenize(source)?;
    let mut parser = Parser { tokens, index: 0 };
    let expression = parser.expression()?;
    while parser.peek() == &Token::Semicolon {
        parser.index += 1;
    }
    if parser.peek() != &Token::Eof {
        return Err(ParseError::TrailingInput);
    }
    Ok(expression)
}

struct Parser {
    tokens: Vec<Token>,
    index: usize,
}

impl Parser {
    fn peek(&self) -> &Token {
        self.tokens.get(self.index).unwrap_or(&Token::Eof)
    }

    fn next(&mut self) -> Token {
        let token = self.peek().clone();
        if self.index < self.tokens.len() {
            self.index += 1;
        }
        token
    }

    fn expression(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.atom()?;
        while self.peek() == &Token::Plus {
            self.next();
            let right = self.atom()?;
            left = Expr::Add(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn atom(&mut self) -> Result<Expr, ParseError> {
        match self.next() {
            Token::Int(value) => Ok(Expr::Int(value)),
            Token::Eof => Err(ParseError::UnexpectedEnd),
            _ => Err(ParseError::ExpectedInteger),
        }
    }
}

pub fn compile(expression: &Expr) -> Vec<Instruction> {
    let mut instructions = Vec::new();
    emit(expression, &mut instructions);
    instructions.push(Instruction::return_());
    instructions
}

fn emit(expression: &Expr, instructions: &mut Vec<Instruction>) {
    match expression {
        Expr::Int(value) => instructions.push(Instruction::load_int(*value)),
        Expr::Add(left, right) => {
            emit(left, instructions);
            emit(right, instructions);
            instructions.push(Instruction::add());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_addition() {
        assert_eq!(
            parse("1+1").unwrap(),
            Expr::Add(Box::new(Expr::Int(1)), Box::new(Expr::Int(1)))
        );
    }

    #[test]
    fn addition_is_left_associative() {
        assert_eq!(
            parse("2+3+4").unwrap(),
            Expr::Add(
                Box::new(Expr::Add(Box::new(Expr::Int(2)), Box::new(Expr::Int(3)))),
                Box::new(Expr::Int(4))
            )
        );
    }

    #[test]
    fn compiles_to_stack_bytecode() {
        let instructions = compile(&parse("1+1").unwrap());
        assert_eq!(
            instructions,
            vec![
                Instruction::load_int(1),
                Instruction::load_int(1),
                Instruction::add(),
                Instruction::return_()
            ]
        );
    }

    #[test]
    fn rejects_unknown_input() {
        assert_eq!(parse("1*1"), Err(ParseError::Unexpected('*')));
    }
}
