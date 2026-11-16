//! R8 front end: lexer, parser, AST, and bytecode compiler.
//!
//! Phase 1 compiles the full expression language — arithmetic, comparison,
//! equality, unary operators, numeric/boolean/null literals, and the
//! `console.log` intrinsic — with real precedence climbing. The parser is
//! deliberately shaped like the real ES2015+ front end: tokens, precedence,
//! AST, then bytecode.

use r8_abi::Instruction;
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    EqStrict,
    NeStrict,
}

impl BinaryOp {
    pub const fn opcode(self) -> u32 {
        match self {
            Self::Add => r8_abi::OP_ADD,
            Self::Sub => r8_abi::OP_SUB,
            Self::Mul => r8_abi::OP_MUL,
            Self::Div => r8_abi::OP_DIV,
            Self::Mod => r8_abi::OP_MOD,
            Self::Pow => r8_abi::OP_POW,
            Self::Lt => r8_abi::OP_LT,
            Self::Gt => r8_abi::OP_GT,
            Self::Le => r8_abi::OP_LE,
            Self::Ge => r8_abi::OP_GE,
            Self::Eq => r8_abi::OP_EQ,
            Self::Ne => r8_abi::OP_NE,
            Self::EqStrict => r8_abi::OP_EQ_STRICT,
            Self::NeStrict => r8_abi::OP_NE_STRICT,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Plus,
    Not,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
    Undefined,
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    Unary(UnaryOp, Box<Expr>),
    /// The `console.log` intrinsic, recognized syntactically until a general
    /// call mechanism exists (Phase 4).
    ConsoleLog(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Int(i64),
    Float(f64),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Pow,
    Bang,
    Lt,
    Gt,
    Le,
    Ge,
    EqEq,
    NotEq,
    EqEqEq,
    NotEqEq,
    LParen,
    RParen,
    Dot,
    Comma,
    Semicolon,
    Identifier(String),
    True,
    False,
    Null,
    Undefined,
    Eof,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("unexpected character '{0}'")]
    Unexpected(char),
    #[error("unexpected end of input")]
    UnexpectedEnd,
    #[error("expected an expression")]
    ExpectedExpression,
    #[error("trailing input")]
    TrailingInput,
    #[error("{0} are not supported yet")]
    Unsupported(&'static str),
    #[error("integer literal too large")]
    IntegerOverflow,
    #[error("malformed number literal")]
    MalformedNumber,
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
            '-' => {
                tokens.push(Token::Minus);
                index += 1;
            }
            '*' => {
                if characters.get(index + 1) == Some(&'*') {
                    tokens.push(Token::Pow);
                    index += 2;
                } else {
                    tokens.push(Token::Star);
                    index += 1;
                }
            }
            '/' => {
                // `//` starts a line comment; otherwise it is division.
                if characters.get(index + 1) == Some(&'/') {
                    while index < characters.len() && characters[index] != '\n' {
                        index += 1;
                    }
                } else {
                    tokens.push(Token::Slash);
                    index += 1;
                }
            }
            '%' => {
                tokens.push(Token::Percent);
                index += 1;
            }
            '!' => {
                if characters.get(index + 1) == Some(&'=')
                    && characters.get(index + 2) == Some(&'=')
                {
                    tokens.push(Token::NotEqEq);
                    index += 3;
                } else if characters.get(index + 1) == Some(&'=') {
                    tokens.push(Token::NotEq);
                    index += 2;
                } else {
                    tokens.push(Token::Bang);
                    index += 1;
                }
            }
            '<' => {
                if characters.get(index + 1) == Some(&'=') {
                    tokens.push(Token::Le);
                    index += 2;
                } else {
                    tokens.push(Token::Lt);
                    index += 1;
                }
            }
            '>' => {
                if characters.get(index + 1) == Some(&'=') {
                    tokens.push(Token::Ge);
                    index += 2;
                } else {
                    tokens.push(Token::Gt);
                    index += 1;
                }
            }
            '=' => {
                if characters.get(index + 1) == Some(&'=')
                    && characters.get(index + 2) == Some(&'=')
                {
                    tokens.push(Token::EqEqEq);
                    index += 3;
                } else if characters.get(index + 1) == Some(&'=') {
                    tokens.push(Token::EqEq);
                    index += 2;
                } else {
                    return Err(ParseError::Unexpected('='));
                }
            }
            '(' => {
                tokens.push(Token::LParen);
                index += 1;
            }
            ')' => {
                tokens.push(Token::RParen);
                index += 1;
            }
            '.' => {
                if characters
                    .get(index + 1)
                    .is_some_and(|c| c.is_ascii_digit())
                {
                    tokens.push(lex_number(&characters, &mut index)?);
                } else {
                    tokens.push(Token::Dot);
                    index += 1;
                }
            }
            ',' => {
                tokens.push(Token::Comma);
                index += 1;
            }
            ';' => {
                tokens.push(Token::Semicolon);
                index += 1;
            }
            character if character.is_ascii_digit() => {
                tokens.push(lex_number(&characters, &mut index)?);
            }
            character if is_identifier_start(character) => {
                let identifier = lex_identifier(&characters, &mut index);
                tokens.push(match identifier.as_str() {
                    "true" => Token::True,
                    "false" => Token::False,
                    "null" => Token::Null,
                    "undefined" => Token::Undefined,
                    _ => Token::Identifier(identifier),
                });
            }
            other => return Err(ParseError::Unexpected(other)),
        }
    }
    tokens.push(Token::Eof);
    Ok(tokens)
}

fn is_identifier_start(character: char) -> bool {
    character.is_ascii_alphabetic() || character == '_' || character == '$'
}

fn is_identifier_part(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_' || character == '$'
}

/// Lexes a numeric literal starting at `index` (a digit or a `.` followed by
/// a digit): `digits [. digits] [e [+-] digits]` or `. digits [e ...]`.
fn lex_number(characters: &[char], index: &mut usize) -> Result<Token, ParseError> {
    let mut text = String::new();
    let mut is_float = false;

    while *index < characters.len() && characters[*index].is_ascii_digit() {
        text.push(characters[*index]);
        *index += 1;
    }
    if *index < characters.len()
        && characters[*index] == '.'
        && characters
            .get(*index + 1)
            .is_some_and(|c| c.is_ascii_digit())
    {
        is_float = true;
        text.push('.');
        *index += 1;
        while *index < characters.len() && characters[*index].is_ascii_digit() {
            text.push(characters[*index]);
            *index += 1;
        }
    }
    if *index < characters.len() && (characters[*index] == 'e' || characters[*index] == 'E') {
        let mut cursor = *index + 1;
        let mut sign = None;
        if matches!(characters.get(cursor), Some('+') | Some('-')) {
            sign = Some(characters[cursor]);
            cursor += 1;
        }
        if characters.get(cursor).is_some_and(|c| c.is_ascii_digit()) {
            is_float = true;
            text.push('e');
            if let Some(sign) = sign {
                text.push(sign);
            }
            *index = cursor;
            while *index < characters.len() && characters[*index].is_ascii_digit() {
                text.push(characters[*index]);
                *index += 1;
            }
        } else {
            // A digit run followed by `e` must form an exponent ("1e" is
            // malformed in JS as well).
            return Err(ParseError::MalformedNumber);
        }
    }

    if is_float {
        let value: f64 = text.parse().map_err(|_| ParseError::MalformedNumber)?;
        return Ok(Token::Float(value));
    }
    let mut value: i64 = 0;
    for character in text.chars() {
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add(character as i64 - '0' as i64))
            .ok_or(ParseError::IntegerOverflow)?;
    }
    Ok(Token::Int(value))
}

fn lex_identifier(characters: &[char], index: &mut usize) -> String {
    let mut text = String::new();
    while *index < characters.len() && is_identifier_part(characters[*index]) {
        text.push(characters[*index]);
        *index += 1;
    }
    text
}

/// Parses a single expression (the `eval` form).
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

/// Parses a program: a sequence of expression statements separated by
/// semicolons. Statements without separators are rejected (no ASI yet).
pub fn parse_program(source: &str) -> Result<Vec<Expr>, ParseError> {
    let tokens = tokenize(source)?;
    let mut parser = Parser { tokens, index: 0 };
    let mut program = Vec::new();
    loop {
        while parser.peek() == &Token::Semicolon {
            parser.index += 1;
        }
        if parser.peek() == &Token::Eof {
            break;
        }
        program.push(parser.expression()?);
        match parser.peek() {
            Token::Semicolon => {}
            Token::Eof => break,
            _ => return Err(ParseError::TrailingInput),
        }
    }
    Ok(program)
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

    // Precedence chain, loosest to tightest:
    // equality → relational → additive → multiplicative → pow → unary → primary

    fn expression(&mut self) -> Result<Expr, ParseError> {
        self.equality()
    }

    fn equality(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.relational()?;
        loop {
            let op = match self.peek() {
                Token::EqEq => BinaryOp::Eq,
                Token::NotEq => BinaryOp::Ne,
                Token::EqEqEq => BinaryOp::EqStrict,
                Token::NotEqEq => BinaryOp::NeStrict,
                _ => break,
            };
            self.index += 1;
            let right = self.relational()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn relational(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.additive()?;
        loop {
            let op = match self.peek() {
                Token::Lt => BinaryOp::Lt,
                Token::Gt => BinaryOp::Gt,
                Token::Le => BinaryOp::Le,
                Token::Ge => BinaryOp::Ge,
                _ => break,
            };
            self.index += 1;
            let right = self.additive()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn additive(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Token::Plus => BinaryOp::Add,
                Token::Minus => BinaryOp::Sub,
                _ => break,
            };
            self.index += 1;
            let right = self.multiplicative()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn multiplicative(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.pow()?;
        loop {
            let op = match self.peek() {
                Token::Star => BinaryOp::Mul,
                Token::Slash => BinaryOp::Div,
                Token::Percent => BinaryOp::Mod,
                _ => break,
            };
            self.index += 1;
            let right = self.pow()?;
            left = Expr::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// `**` is right-associative: `2**3**2` is `2**(3**2)`. Unlike JS, which
    /// makes `-2**3` a syntax error, unary minus is accepted and binds like
    /// Python's `**` (deviation noted for later alignment).
    fn pow(&mut self) -> Result<Expr, ParseError> {
        let left = self.unary()?;
        if self.peek() == &Token::Pow {
            self.index += 1;
            let right = self.pow()?;
            return Ok(Expr::Binary(BinaryOp::Pow, Box::new(left), Box::new(right)));
        }
        Ok(left)
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        let op = match self.peek() {
            Token::Minus => UnaryOp::Neg,
            Token::Plus => UnaryOp::Plus,
            Token::Bang => UnaryOp::Not,
            _ => return self.primary(),
        };
        self.index += 1;
        let operand = self.unary()?;
        Ok(Expr::Unary(op, Box::new(operand)))
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        match self.next() {
            Token::Int(value) => Ok(Expr::Int(value)),
            Token::Float(value) => Ok(Expr::Float(value)),
            Token::True => Ok(Expr::Bool(true)),
            Token::False => Ok(Expr::Bool(false)),
            Token::Null => Ok(Expr::Null),
            Token::Undefined => Ok(Expr::Undefined),
            Token::LParen => {
                let expression = self.expression()?;
                match self.next() {
                    Token::RParen => Ok(expression),
                    Token::Eof => Err(ParseError::UnexpectedEnd),
                    _ => Err(ParseError::TrailingInput),
                }
            }
            Token::Identifier(name) if name == "console" => self.console_log(),
            Token::Identifier(_) => Err(ParseError::Unsupported("variable references")),
            Token::Eof => Err(ParseError::UnexpectedEnd),
            _ => Err(ParseError::ExpectedExpression),
        }
    }

    /// Recognizes `console.log(expression)` — the Phase 1 print intrinsic.
    fn console_log(&mut self) -> Result<Expr, ParseError> {
        if self.next() != Token::Dot {
            return Err(ParseError::Unsupported("only console.log calls"));
        }
        match self.next() {
            Token::Identifier(name) if name == "log" => {}
            Token::Eof => return Err(ParseError::UnexpectedEnd),
            _ => return Err(ParseError::Unsupported("only console.log calls")),
        }
        match self.next() {
            Token::LParen => {}
            Token::Eof => return Err(ParseError::UnexpectedEnd),
            _ => return Err(ParseError::TrailingInput),
        }
        let argument = self.expression()?;
        match self.next() {
            Token::RParen => {}
            Token::Comma => return Err(ParseError::Unsupported("multiple console.log arguments")),
            Token::Eof => return Err(ParseError::UnexpectedEnd),
            _ => return Err(ParseError::TrailingInput),
        }
        Ok(Expr::ConsoleLog(Box::new(argument)))
    }
}

pub fn compile(expression: &Expr) -> Vec<Instruction> {
    let mut instructions = Vec::new();
    emit(expression, &mut instructions);
    instructions.push(Instruction::return_());
    instructions
}

/// Compiles a program: every statement's value is discarded with `OP_POP`
/// except the last, whose value the machine returns.
pub fn compile_program(program: &[Expr]) -> Vec<Instruction> {
    let mut instructions = Vec::new();
    for (index, statement) in program.iter().enumerate() {
        emit(statement, &mut instructions);
        if index + 1 == program.len() {
            instructions.push(Instruction::return_());
        } else {
            instructions.push(Instruction::pop());
        }
    }
    instructions
}

fn emit(expression: &Expr, instructions: &mut Vec<Instruction>) {
    match expression {
        Expr::Int(value) => {
            if *value >= i32::MIN as i64 && *value <= i32::MAX as i64 {
                instructions.push(Instruction::load_int(*value));
            } else {
                // Integers beyond `i32` go through the float path: exact up
                // to 2^53, matching JS double semantics.
                instructions.extend(Instruction::load_f64(*value as f64));
            }
        }
        Expr::Float(value) => instructions.extend(Instruction::load_f64(*value)),
        Expr::Bool(value) => instructions.push(Instruction::load_bool(*value)),
        Expr::Null => instructions.push(Instruction::load_null()),
        Expr::Undefined => instructions.push(Instruction::load_undefined()),
        Expr::Binary(op, left, right) => {
            emit(left, instructions);
            emit(right, instructions);
            instructions.push(Instruction::binary(op.opcode()));
        }
        Expr::Unary(op, operand) => {
            emit(operand, instructions);
            match op {
                UnaryOp::Neg => instructions.push(Instruction::unary(r8_abi::OP_NEG)),
                UnaryOp::Not => instructions.push(Instruction::unary(r8_abi::OP_NOT)),
                // Unary plus is numeric coercion. Current operand types are
                // numbers except booleans (`+true` should be 1 — Phase 2 TODO).
                UnaryOp::Plus => {}
            }
        }
        Expr::ConsoleLog(argument) => {
            emit(argument, instructions);
            instructions.push(Instruction::print());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r8_abi::{OP_ADD, OP_RETURN};

    #[test]
    fn parses_addition() {
        assert_eq!(
            parse("1+1").unwrap(),
            Expr::Binary(
                BinaryOp::Add,
                Box::new(Expr::Int(1)),
                Box::new(Expr::Int(1))
            )
        );
    }

    #[test]
    fn addition_is_left_associative() {
        assert_eq!(
            parse("2+3+4").unwrap(),
            Expr::Binary(
                BinaryOp::Add,
                Box::new(Expr::Binary(
                    BinaryOp::Add,
                    Box::new(Expr::Int(2)),
                    Box::new(Expr::Int(3))
                )),
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
                Instruction::binary(OP_ADD),
                Instruction::return_()
            ]
        );
    }

    #[test]
    fn rejects_unknown_input() {
        assert_eq!(parse("1@1"), Err(ParseError::Unexpected('@')));
    }

    #[test]
    fn lexes_numbers() {
        assert_eq!(tokenize("42").unwrap(), vec![Token::Int(42), Token::Eof]);
        assert_eq!(
            tokenize("1.5").unwrap(),
            vec![Token::Float(1.5), Token::Eof]
        );
        assert_eq!(tokenize(".5").unwrap(), vec![Token::Float(0.5), Token::Eof]);
        assert_eq!(
            tokenize("1e3").unwrap(),
            vec![Token::Float(1000.0), Token::Eof]
        );
        assert_eq!(
            tokenize("2e-2").unwrap(),
            vec![Token::Float(0.02), Token::Eof]
        );
        assert_eq!(
            tokenize("1.5e+2").unwrap(),
            vec![Token::Float(150.0), Token::Eof]
        );
    }

    #[test]
    fn lexes_operators_with_maximal_munch() {
        assert_eq!(
            tokenize("2**3<=4===true!=false!==null").unwrap(),
            vec![
                Token::Int(2),
                Token::Pow,
                Token::Int(3),
                Token::Le,
                Token::Int(4),
                Token::EqEqEq,
                Token::True,
                Token::NotEq,
                Token::False,
                Token::NotEqEq,
                Token::Null,
                Token::Eof
            ]
        );
    }

    #[test]
    fn lexes_line_comments() {
        assert_eq!(
            tokenize("1 + 2 // the rest is a comment\n+ 3").unwrap(),
            vec![
                Token::Int(1),
                Token::Plus,
                Token::Int(2),
                Token::Plus,
                Token::Int(3),
                Token::Eof
            ]
        );
    }

    #[test]
    fn rejects_integer_overflow() {
        assert_eq!(
            parse("99999999999999999999"),
            Err(ParseError::IntegerOverflow)
        );
    }

    #[test]
    fn rejects_trailing_exponent() {
        assert_eq!(parse("1e"), Err(ParseError::MalformedNumber));
        assert_eq!(parse("1e+"), Err(ParseError::MalformedNumber));
    }

    #[test]
    fn respects_operator_precedence() {
        // * binds tighter than +
        assert_eq!(
            parse("2+3*4").unwrap(),
            Expr::Binary(
                BinaryOp::Add,
                Box::new(Expr::Int(2)),
                Box::new(Expr::Binary(
                    BinaryOp::Mul,
                    Box::new(Expr::Int(3)),
                    Box::new(Expr::Int(4))
                ))
            )
        );
        // comparison binds tighter than equality
        assert_eq!(
            parse("2<3==true").unwrap(),
            Expr::Binary(
                BinaryOp::Eq,
                Box::new(Expr::Binary(
                    BinaryOp::Lt,
                    Box::new(Expr::Int(2)),
                    Box::new(Expr::Int(3))
                )),
                Box::new(Expr::Bool(true))
            )
        );
        // parens override everything
        assert_eq!(
            parse("(2+3)*4").unwrap(),
            Expr::Binary(
                BinaryOp::Mul,
                Box::new(Expr::Binary(
                    BinaryOp::Add,
                    Box::new(Expr::Int(2)),
                    Box::new(Expr::Int(3))
                )),
                Box::new(Expr::Int(4))
            )
        );
    }

    #[test]
    fn pow_is_right_associative() {
        assert_eq!(
            parse("2**3**2").unwrap(),
            Expr::Binary(
                BinaryOp::Pow,
                Box::new(Expr::Int(2)),
                Box::new(Expr::Binary(
                    BinaryOp::Pow,
                    Box::new(Expr::Int(3)),
                    Box::new(Expr::Int(2))
                ))
            )
        );
    }

    #[test]
    fn multiplicative_is_left_associative() {
        assert_eq!(
            parse("7*3%2").unwrap(),
            Expr::Binary(
                BinaryOp::Mod,
                Box::new(Expr::Binary(
                    BinaryOp::Mul,
                    Box::new(Expr::Int(7)),
                    Box::new(Expr::Int(3))
                )),
                Box::new(Expr::Int(2))
            )
        );
    }

    #[test]
    fn unary_binds_tighter_than_binary() {
        assert_eq!(
            parse("-2*3").unwrap(),
            Expr::Binary(
                BinaryOp::Mul,
                Box::new(Expr::Unary(UnaryOp::Neg, Box::new(Expr::Int(2)))),
                Box::new(Expr::Int(3))
            )
        );
        assert_eq!(
            parse("!!true").unwrap(),
            Expr::Unary(
                UnaryOp::Not,
                Box::new(Expr::Unary(UnaryOp::Not, Box::new(Expr::Bool(true))))
            )
        );
    }

    #[test]
    fn parses_console_log() {
        assert_eq!(
            parse("console.log(1+1)").unwrap(),
            Expr::ConsoleLog(Box::new(Expr::Binary(
                BinaryOp::Add,
                Box::new(Expr::Int(1)),
                Box::new(Expr::Int(1))
            )))
        );
    }

    #[test]
    fn rejects_unsupported_features() {
        assert_eq!(
            parse("name").unwrap_err(),
            ParseError::Unsupported("variable references")
        );
        assert_eq!(
            parse("console.log(1,2)").unwrap_err(),
            ParseError::Unsupported("multiple console.log arguments")
        );
        assert_eq!(
            parse("console.error(1)").unwrap_err(),
            ParseError::Unsupported("only console.log calls")
        );
    }

    #[test]
    fn parses_programs() {
        let source = "console.log(1 + 1);\nconsole.log(2 + 3 + 4);";
        let program = parse_program(source).unwrap();
        assert_eq!(program.len(), 2);
        assert_eq!(program[0], parse("console.log(1+1)").unwrap());

        assert!(parse_program("").unwrap().is_empty());
        assert!(parse_program("1;").unwrap().len() == 1);
        assert!(parse_program(";; 1 ;;").unwrap().len() == 1);
        assert_eq!(parse_program("1 2").unwrap_err(), ParseError::TrailingInput);
    }

    #[test]
    fn compiles_float_literal_to_two_instructions() {
        let instructions = compile(&parse("1.5").unwrap());
        assert_eq!(instructions.len(), 3);
        assert_eq!(instructions[0].op, r8_abi::OP_LOAD_F64_HI);
        assert_eq!(instructions[1].op, r8_abi::OP_LOAD_F64);
        assert_eq!(instructions[2].op, OP_RETURN);
    }

    #[test]
    fn compiles_large_integers_through_the_float_path() {
        let instructions = compile(&parse("5000000000").unwrap());
        assert_eq!(instructions[0].op, r8_abi::OP_LOAD_F64_HI);
    }

    #[test]
    fn compiles_program_with_pops_and_final_return() {
        let program = parse_program("1; 2; 3").unwrap();
        let instructions = compile_program(&program);
        assert_eq!(
            instructions,
            vec![
                Instruction::load_int(1),
                Instruction::pop(),
                Instruction::load_int(2),
                Instruction::pop(),
                Instruction::load_int(3),
                Instruction::return_()
            ]
        );
    }

    #[test]
    fn compiles_console_log_to_print() {
        let instructions = compile(&parse("console.log(2+3)").unwrap());
        assert_eq!(
            instructions,
            vec![
                Instruction::load_int(2),
                Instruction::load_int(3),
                Instruction::binary(OP_ADD),
                Instruction::print(),
                Instruction::return_()
            ]
        );
        assert_eq!(instructions[3].op, r8_abi::OP_PRINT);
    }
}
