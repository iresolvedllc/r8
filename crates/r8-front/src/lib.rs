//! R8 front end: lexer, parser, AST, and bytecode compiler.
//!
//! Phase 3 adds strings: string literals with escapes, template literals
//! with nested substitutions, a constant pool for string literals (interned
//! by value), and `OP_TO_STRING` for substitutions. The compiler output is a
//! `Compiled` (bytecode + pool) because the machine now takes both.
//!
//! Phase 2 compiled statements: `let`/`const`/`var` declarations, assignment,
//! blocks, `if`/`else`, `while`, `for`, and short-circuit `&&`/`||`, with a
//! scope-aware compiler that gives every declaration its own local slot and
//! patches absolute-index jumps.

use r8_abi::{Instruction, Value};
use std::collections::HashMap;
use thiserror::Error;

/// Local slot count, mirrored from the machine's `LOCAL_COUNT` in
/// `zig/src/interp.zig`.
pub const LOCAL_COUNT: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignOp {
    Assign,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    ModAssign,
}

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
    LogicalAnd,
    LogicalOr,
}

impl BinaryOp {
    /// The plain opcode for arithmetic/comparison ops. Logical operators do
    /// not have one: they compile to `OP_DUP` + conditional jumps instead.
    pub const fn opcode(self) -> Option<u32> {
        match self {
            Self::Add => Some(r8_abi::OP_ADD),
            Self::Sub => Some(r8_abi::OP_SUB),
            Self::Mul => Some(r8_abi::OP_MUL),
            Self::Div => Some(r8_abi::OP_DIV),
            Self::Mod => Some(r8_abi::OP_MOD),
            Self::Pow => Some(r8_abi::OP_POW),
            Self::Lt => Some(r8_abi::OP_LT),
            Self::Gt => Some(r8_abi::OP_GT),
            Self::Le => Some(r8_abi::OP_LE),
            Self::Ge => Some(r8_abi::OP_GE),
            Self::Eq => Some(r8_abi::OP_EQ),
            Self::Ne => Some(r8_abi::OP_NE),
            Self::EqStrict => Some(r8_abi::OP_EQ_STRICT),
            Self::NeStrict => Some(r8_abi::OP_NE_STRICT),
            Self::LogicalAnd | Self::LogicalOr => None,
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
pub enum TemplatePart {
    Text(String),
    Substitution(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
    Undefined,
    Var(String),
    String(String),
    Template(Vec<TemplatePart>),
    Assign(AssignOp, String, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    Unary(UnaryOp, Box<Expr>),
    /// The `console.log` intrinsic, recognized syntactically until a general
    /// call mechanism exists (Phase 4).
    ConsoleLog(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Declarator {
    pub name: String,
    pub init: Option<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Expression(Expr),
    Empty,
    /// `let`/`var` declarations; both are block-scoped slot declarations in
    /// this phase (hoisting differences arrive with functions).
    Let(Vec<Declarator>),
    Const(Vec<Declarator>),
    Block(Vec<Stmt>),
    If {
        condition: Expr,
        then_body: Box<Stmt>,
        else_body: Option<Box<Stmt>>,
    },
    While {
        condition: Expr,
        body: Box<Stmt>,
    },
    For {
        init: Option<Box<Stmt>>,
        condition: Option<Expr>,
        update: Option<Expr>,
        body: Box<Stmt>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Int(i64),
    Float(f64),
    String(String),
    TemplateStart,
    TemplateText(String),
    TemplateExprOpen,
    TemplateExprClose,
    TemplateEnd,
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
    EqAssign,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    AmpAmp,
    PipePipe,
    LParen,
    RParen,
    LeftBrace,
    RightBrace,
    Dot,
    Comma,
    Semicolon,
    Identifier(String),
    Let,
    Var,
    Const,
    If,
    Else,
    While,
    For,
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
    #[error("expected an identifier")]
    ExpectedIdentifier,
    #[error("trailing input")]
    TrailingInput,
    #[error("unterminated string literal")]
    UnterminatedString,
    #[error("invalid escape sequence")]
    InvalidEscape,
    #[error("{0} are not supported yet")]
    Unsupported(&'static str),
    #[error("const declaration requires an initializer")]
    MissingInitializer,
    #[error("integer literal too large")]
    IntegerOverflow,
    #[error("malformed number literal")]
    MalformedNumber,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CompileError {
    #[error("'{0}' is not declared")]
    Undeclared(String),
    #[error("'{0}' is read-only (const)")]
    ConstAssignment(String),
    #[error("too many locals")]
    TooManyLocals,
}

/// Bytecode plus its string-constant pool; both are passed to the machine.
#[derive(Debug, Clone)]
pub struct Compiled {
    pub instructions: Vec<Instruction>,
    pub pool: Vec<Value>,
}

#[derive(Debug, Clone, Copy)]
enum LexMode {
    Normal,
    TemplateText,
    TemplateExpr { brace_depth: usize },
}

pub fn tokenize(source: &str) -> Result<Vec<Token>, ParseError> {
    let characters: Vec<char> = source.chars().collect();
    let mut lexer = Lexer {
        characters: &characters,
        index: 0,
        tokens: Vec::new(),
        modes: vec![LexMode::Normal],
    };
    while lexer.index < lexer.characters.len() {
        lexer.step()?;
    }
    lexer.tokens.push(Token::Eof);
    Ok(lexer.tokens)
}

struct Lexer<'a> {
    characters: &'a [char],
    index: usize,
    tokens: Vec<Token>,
    modes: Vec<LexMode>,
}

impl Lexer<'_> {
    fn step(&mut self) -> Result<(), ParseError> {
        let character = self.characters[self.index];
        match *self.modes.last().expect("a mode is always active") {
            LexMode::TemplateText => {
                self.template_text_step()?;
            }
            LexMode::TemplateExpr { .. } => match character {
                '{' => {
                    self.tokens.push(Token::LeftBrace);
                    if let Some(LexMode::TemplateExpr { brace_depth }) = self.modes.last_mut() {
                        *brace_depth += 1;
                    }
                    self.index += 1;
                }
                '}' => {
                    let at_top = matches!(
                        self.modes.last(),
                        Some(LexMode::TemplateExpr { brace_depth: 0 })
                    );
                    if at_top {
                        self.tokens.push(Token::TemplateExprClose);
                        self.modes.pop();
                    } else {
                        self.tokens.push(Token::RightBrace);
                        if let Some(LexMode::TemplateExpr { brace_depth }) = self.modes.last_mut() {
                            *brace_depth -= 1;
                        }
                    }
                    self.index += 1;
                }
                '`' => {
                    // A nested template inside a substitution.
                    self.tokens.push(Token::TemplateStart);
                    self.modes.push(LexMode::TemplateText);
                    self.index += 1;
                }
                _ => self.common_step()?,
            },
            LexMode::Normal => match character {
                '`' => {
                    self.tokens.push(Token::TemplateStart);
                    self.modes.push(LexMode::TemplateText);
                    self.index += 1;
                }
                _ => self.common_step()?,
            },
        }
        Ok(())
    }

    /// Consumes a run of cooked template text, ending at a closing backtick
    /// (emits `TemplateEnd`) or at `${` (emits `TemplateExprOpen`).
    fn template_text_step(&mut self) -> Result<(), ParseError> {
        let mut text = String::new();
        loop {
            let character = *self
                .characters
                .get(self.index)
                .ok_or(ParseError::UnterminatedString)?;
            match character {
                '`' => {
                    self.index += 1;
                    if !text.is_empty() {
                        self.tokens.push(Token::TemplateText(text));
                    }
                    self.tokens.push(Token::TemplateEnd);
                    self.modes.pop();
                    return Ok(());
                }
                '$' if self.characters.get(self.index + 1) == Some(&'{') => {
                    self.index += 2;
                    if !text.is_empty() {
                        self.tokens.push(Token::TemplateText(text));
                    }
                    self.tokens.push(Token::TemplateExprOpen);
                    self.modes.push(LexMode::TemplateExpr { brace_depth: 0 });
                    return Ok(());
                }
                '\\' => {
                    let decoded = self.decode_escape(true)?;
                    text.push(decoded);
                }
                _ => {
                    // Newlines are legal inside template literals.
                    text.push(character);
                    self.index += 1;
                }
            }
        }
    }

    /// Lexes a `'...'` or `"..."` string literal with escapes. Raw newlines
    /// are illegal, as in JS.
    fn string_literal(&mut self) -> Result<(), ParseError> {
        let quote = self.characters[self.index];
        self.index += 1;
        let mut value = String::new();
        loop {
            let character = *self
                .characters
                .get(self.index)
                .ok_or(ParseError::UnterminatedString)?;
            match character {
                c if c == quote => {
                    self.index += 1;
                    break;
                }
                '\n' | '\r' => return Err(ParseError::UnterminatedString),
                '\\' => {
                    let decoded = self.decode_escape(false)?;
                    value.push(decoded);
                }
                other => {
                    value.push(other);
                    self.index += 1;
                }
            }
        }
        self.tokens.push(Token::String(value));
        Ok(())
    }

    /// Decodes an escape sequence after the backslash, advancing the index.
    fn decode_escape(&mut self, in_template: bool) -> Result<char, ParseError> {
        self.index += 1;
        let escape = *self
            .characters
            .get(self.index)
            .ok_or(ParseError::UnterminatedString)?;
        self.index += 1;
        let decoded = match escape {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            'b' => '\u{8}',
            'f' => '\u{c}',
            'v' => '\u{b}',
            '0' => '\0',
            '\\' => '\\',
            '\'' => '\'',
            '"' => '"',
            '`' if in_template => '`',
            '$' if in_template => '$',
            'x' => {
                let hex = self.hex_digits(2)?;
                char::from_u32(hex).ok_or(ParseError::InvalidEscape)?
            }
            'u' => {
                if self.characters.get(self.index) == Some(&'{') {
                    self.index += 1;
                    let mut hex = 0u32;
                    let mut digits = 0;
                    while let Some(&c) = self.characters.get(self.index) {
                        if c == '}' {
                            break;
                        }
                        let digit = c.to_digit(16).ok_or(ParseError::InvalidEscape)?;
                        hex = hex
                            .checked_mul(16)
                            .and_then(|h| h.checked_add(digit))
                            .ok_or(ParseError::InvalidEscape)?;
                        digits += 1;
                        self.index += 1;
                    }
                    if self.characters.get(self.index) != Some(&'}') || digits == 0 {
                        return Err(ParseError::UnterminatedString);
                    }
                    self.index += 1;
                    char::from_u32(hex).ok_or(ParseError::InvalidEscape)?
                } else {
                    let hex = self.hex_digits(4)?;
                    char::from_u32(hex).ok_or(ParseError::InvalidEscape)?
                }
            }
            // Identity escape: JS allows `\<char>` for most characters.
            other => other,
        };
        Ok(decoded)
    }

    fn hex_digits(&mut self, count: usize) -> Result<u32, ParseError> {
        let mut hex = 0u32;
        for _ in 0..count {
            let character = *self
                .characters
                .get(self.index)
                .ok_or(ParseError::InvalidEscape)?;
            let digit = character.to_digit(16).ok_or(ParseError::InvalidEscape)?;
            hex = hex * 16 + digit;
            self.index += 1;
        }
        Ok(hex)
    }

    /// The mode-independent token recognition shared by `Normal` and
    /// `TemplateExpr` (backticks are handled by the callers).
    fn common_step(&mut self) -> Result<(), ParseError> {
        let character = self.characters[self.index];
        match character {
            c if c.is_whitespace() => {
                self.index += 1;
            }
            '"' | '\'' => self.string_literal()?,
            '+' => {
                if self.characters.get(self.index + 1) == Some(&'=') {
                    self.tokens.push(Token::PlusEq);
                    self.index += 2;
                } else {
                    self.tokens.push(Token::Plus);
                    self.index += 1;
                }
            }
            '-' => {
                if self.characters.get(self.index + 1) == Some(&'=') {
                    self.tokens.push(Token::MinusEq);
                    self.index += 2;
                } else {
                    self.tokens.push(Token::Minus);
                    self.index += 1;
                }
            }
            '*' => {
                if self.characters.get(self.index + 1) == Some(&'*') {
                    self.tokens.push(Token::Pow);
                    self.index += 2;
                } else if self.characters.get(self.index + 1) == Some(&'=') {
                    self.tokens.push(Token::StarEq);
                    self.index += 2;
                } else {
                    self.tokens.push(Token::Star);
                    self.index += 1;
                }
            }
            '/' => {
                // `//` starts a line comment; otherwise it is division.
                if self.characters.get(self.index + 1) == Some(&'/') {
                    while self.index < self.characters.len() && self.characters[self.index] != '\n'
                    {
                        self.index += 1;
                    }
                } else if self.characters.get(self.index + 1) == Some(&'=') {
                    self.tokens.push(Token::SlashEq);
                    self.index += 2;
                } else {
                    self.tokens.push(Token::Slash);
                    self.index += 1;
                }
            }
            '%' => {
                if self.characters.get(self.index + 1) == Some(&'=') {
                    self.tokens.push(Token::PercentEq);
                    self.index += 2;
                } else {
                    self.tokens.push(Token::Percent);
                    self.index += 1;
                }
            }
            '!' => {
                if self.characters.get(self.index + 1) == Some(&'=')
                    && self.characters.get(self.index + 2) == Some(&'=')
                {
                    self.tokens.push(Token::NotEqEq);
                    self.index += 3;
                } else if self.characters.get(self.index + 1) == Some(&'=') {
                    self.tokens.push(Token::NotEq);
                    self.index += 2;
                } else {
                    self.tokens.push(Token::Bang);
                    self.index += 1;
                }
            }
            '&' => {
                if self.characters.get(self.index + 1) == Some(&'&') {
                    self.tokens.push(Token::AmpAmp);
                    self.index += 2;
                } else {
                    // Bitwise `&` is not supported yet.
                    return Err(ParseError::Unexpected('&'));
                }
            }
            '|' => {
                if self.characters.get(self.index + 1) == Some(&'|') {
                    self.tokens.push(Token::PipePipe);
                    self.index += 2;
                } else {
                    // Bitwise `|` is not supported yet.
                    return Err(ParseError::Unexpected('|'));
                }
            }
            '<' => {
                if self.characters.get(self.index + 1) == Some(&'=') {
                    self.tokens.push(Token::Le);
                    self.index += 2;
                } else {
                    self.tokens.push(Token::Lt);
                    self.index += 1;
                }
            }
            '>' => {
                if self.characters.get(self.index + 1) == Some(&'=') {
                    self.tokens.push(Token::Ge);
                    self.index += 2;
                } else {
                    self.tokens.push(Token::Gt);
                    self.index += 1;
                }
            }
            '=' => {
                if self.characters.get(self.index + 1) == Some(&'=')
                    && self.characters.get(self.index + 2) == Some(&'=')
                {
                    self.tokens.push(Token::EqEqEq);
                    self.index += 3;
                } else if self.characters.get(self.index + 1) == Some(&'=') {
                    self.tokens.push(Token::EqEq);
                    self.index += 2;
                } else {
                    self.tokens.push(Token::EqAssign);
                    self.index += 1;
                }
            }
            '(' => {
                self.tokens.push(Token::LParen);
                self.index += 1;
            }
            ')' => {
                self.tokens.push(Token::RParen);
                self.index += 1;
            }
            '{' => {
                self.tokens.push(Token::LeftBrace);
                self.index += 1;
            }
            '}' => {
                self.tokens.push(Token::RightBrace);
                self.index += 1;
            }
            '.' => {
                if self
                    .characters
                    .get(self.index + 1)
                    .is_some_and(|c| c.is_ascii_digit())
                {
                    self.tokens
                        .push(lex_number(self.characters, &mut self.index)?);
                } else {
                    self.tokens.push(Token::Dot);
                    self.index += 1;
                }
            }
            ',' => {
                self.tokens.push(Token::Comma);
                self.index += 1;
            }
            ';' => {
                self.tokens.push(Token::Semicolon);
                self.index += 1;
            }
            c if c.is_ascii_digit() => {
                self.tokens
                    .push(lex_number(self.characters, &mut self.index)?);
            }
            c if is_identifier_start(c) => {
                let identifier = lex_identifier(self.characters, &mut self.index);
                self.tokens.push(match identifier.as_str() {
                    "let" => Token::Let,
                    "var" => Token::Var,
                    "const" => Token::Const,
                    "if" => Token::If,
                    "else" => Token::Else,
                    "while" => Token::While,
                    "for" => Token::For,
                    "true" => Token::True,
                    "false" => Token::False,
                    "null" => Token::Null,
                    "undefined" => Token::Undefined,
                    _ => Token::Identifier(identifier),
                });
            }
            other => return Err(ParseError::Unexpected(other)),
        }
        Ok(())
    }
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

/// Parses a program: a sequence of statements. Semicolons terminate
/// expression statements; a statement before `}` or end of input does not
/// need one (a deliberately small slice of ASI).
pub fn parse_program(source: &str) -> Result<Vec<Stmt>, ParseError> {
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
        program.push(parser.statement()?);
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

    fn expect(&mut self, token: &Token) -> Result<(), ParseError> {
        if self.peek() == token {
            self.index += 1;
            Ok(())
        } else if self.peek() == &Token::Eof {
            Err(ParseError::UnexpectedEnd)
        } else {
            Err(ParseError::TrailingInput)
        }
    }

    // Precedence chain, loosest to tightest:
    // assignment → logical or → logical and → equality → relational →
    // additive → multiplicative → pow → unary → primary

    fn expression(&mut self) -> Result<Expr, ParseError> {
        self.assignment()
    }

    fn assignment(&mut self) -> Result<Expr, ParseError> {
        let left = self.logical_or()?;
        let op = match self.peek() {
            Token::EqAssign => AssignOp::Assign,
            Token::PlusEq => AssignOp::AddAssign,
            Token::MinusEq => AssignOp::SubAssign,
            Token::StarEq => AssignOp::MulAssign,
            Token::SlashEq => AssignOp::DivAssign,
            Token::PercentEq => AssignOp::ModAssign,
            _ => return Ok(left),
        };
        self.index += 1;
        let target = match left {
            Expr::Var(name) => name,
            _ => return Err(ParseError::Unsupported("only simple assignment targets")),
        };
        let value = self.assignment()?;
        Ok(Expr::Assign(op, target, Box::new(value)))
    }

    fn logical_or(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.logical_and()?;
        while self.peek() == &Token::PipePipe {
            self.index += 1;
            let right = self.logical_and()?;
            left = Expr::Binary(BinaryOp::LogicalOr, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn logical_and(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.equality()?;
        while self.peek() == &Token::AmpAmp {
            self.index += 1;
            let right = self.equality()?;
            left = Expr::Binary(BinaryOp::LogicalAnd, Box::new(left), Box::new(right));
        }
        Ok(left)
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
            Token::String(text) => Ok(Expr::String(text)),
            Token::TemplateStart => self.template(),
            Token::True => Ok(Expr::Bool(true)),
            Token::False => Ok(Expr::Bool(false)),
            Token::Null => Ok(Expr::Null),
            Token::Undefined => Ok(Expr::Undefined),
            Token::Identifier(name) if name == "console" => self.console_log(),
            Token::Identifier(name) => Ok(Expr::Var(name)),
            Token::LParen => {
                let expression = self.expression()?;
                match self.next() {
                    Token::RParen => Ok(expression),
                    Token::Eof => Err(ParseError::UnexpectedEnd),
                    _ => Err(ParseError::TrailingInput),
                }
            }
            Token::Eof => Err(ParseError::UnexpectedEnd),
            _ => Err(ParseError::ExpectedExpression),
        }
    }

    /// Parses `` `text ${expr} text` `` into text/substitution parts, after
    /// the opening `TemplateStart` and through the closing `TemplateEnd`.
    fn template(&mut self) -> Result<Expr, ParseError> {
        let mut parts = Vec::new();
        loop {
            match self.next() {
                Token::TemplateText(text) => parts.push(TemplatePart::Text(text)),
                Token::TemplateExprOpen => {
                    let expression = self.expression()?;
                    match self.next() {
                        Token::TemplateExprClose => {
                            parts.push(TemplatePart::Substitution(expression))
                        }
                        Token::Eof => return Err(ParseError::UnexpectedEnd),
                        _ => return Err(ParseError::TrailingInput),
                    }
                }
                Token::TemplateEnd => break,
                Token::Eof => return Err(ParseError::UnexpectedEnd),
                _ => return Err(ParseError::TrailingInput),
            }
        }
        Ok(Expr::Template(parts))
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

    fn statement(&mut self) -> Result<Stmt, ParseError> {
        match self.peek() {
            Token::Let | Token::Var => {
                self.next();
                let declarators = self.declarators(false)?;
                self.end_statement()?;
                Ok(Stmt::Let(declarators))
            }
            Token::Const => {
                self.next();
                let declarators = self.declarators(true)?;
                self.end_statement()?;
                Ok(Stmt::Const(declarators))
            }
            Token::If => self.if_statement(),
            Token::While => self.while_statement(),
            Token::For => self.for_statement(),
            Token::LeftBrace => {
                self.next();
                Ok(Stmt::Block(self.block_body()?))
            }
            Token::Semicolon => {
                self.next();
                Ok(Stmt::Empty)
            }
            _ => {
                let expression = self.expression()?;
                self.end_statement()?;
                Ok(Stmt::Expression(expression))
            }
        }
    }

    fn end_statement(&mut self) -> Result<(), ParseError> {
        match self.peek() {
            Token::Semicolon => {
                self.index += 1;
                Ok(())
            }
            Token::RightBrace | Token::Eof => Ok(()),
            _ => Err(ParseError::TrailingInput),
        }
    }

    fn declarators(&mut self, require_init: bool) -> Result<Vec<Declarator>, ParseError> {
        let mut declarators = vec![self.declarator(require_init)?];
        while self.peek() == &Token::Comma {
            self.index += 1;
            declarators.push(self.declarator(require_init)?);
        }
        Ok(declarators)
    }

    fn declarator(&mut self, require_init: bool) -> Result<Declarator, ParseError> {
        let name = match self.next() {
            Token::Identifier(name) => name,
            Token::Eof => return Err(ParseError::UnexpectedEnd),
            _ => return Err(ParseError::ExpectedIdentifier),
        };
        let init = if self.peek() == &Token::EqAssign {
            self.index += 1;
            Some(self.expression()?)
        } else if require_init {
            return Err(ParseError::MissingInitializer);
        } else {
            None
        };
        Ok(Declarator { name, init })
    }

    fn if_statement(&mut self) -> Result<Stmt, ParseError> {
        self.next(); // `if`
        self.expect(&Token::LParen)?;
        let condition = self.expression()?;
        self.expect(&Token::RParen)?;
        let then_body = Box::new(self.statement()?);
        let else_body = if self.peek() == &Token::Else {
            self.index += 1;
            Some(Box::new(self.statement()?))
        } else {
            None
        };
        Ok(Stmt::If {
            condition,
            then_body,
            else_body,
        })
    }

    fn while_statement(&mut self) -> Result<Stmt, ParseError> {
        self.next(); // `while`
        self.expect(&Token::LParen)?;
        let condition = self.expression()?;
        self.expect(&Token::RParen)?;
        let body = Box::new(self.statement()?);
        Ok(Stmt::While { condition, body })
    }

    fn for_statement(&mut self) -> Result<Stmt, ParseError> {
        self.next(); // `for`
        self.expect(&Token::LParen)?;
        let init = if self.peek() == &Token::Semicolon {
            self.index += 1;
            None
        } else if matches!(self.peek(), Token::Let | Token::Var | Token::Const) {
            // Detect `for (let x of ...)` / `for (let x in ...)` up front for
            // a clear error instead of tripping over the initializer rules.
            if let Some(Token::Identifier(after_name)) = self.tokens.get(self.index + 2)
                && (after_name == "of" || after_name == "in")
            {
                return Err(ParseError::Unsupported("for-of and for-in loops"));
            }
            let is_const = self.peek() == &Token::Const;
            self.next();
            let declarators = self.declarators(is_const)?;
            if let Token::Identifier(name) = self.peek()
                && (name == "of" || name == "in")
            {
                return Err(ParseError::Unsupported("for-of and for-in loops"));
            }
            self.expect(&Token::Semicolon)?;
            Some(Box::new(if is_const {
                Stmt::Const(declarators)
            } else {
                Stmt::Let(declarators)
            }))
        } else {
            let expression = self.expression()?;
            self.expect(&Token::Semicolon)?;
            Some(Box::new(Stmt::Expression(expression)))
        };
        let condition = if self.peek() == &Token::Semicolon {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect(&Token::Semicolon)?;
        let update = if self.peek() == &Token::RParen {
            None
        } else {
            Some(self.expression()?)
        };
        self.expect(&Token::RParen)?;
        let body = Box::new(self.statement()?);
        Ok(Stmt::For {
            init,
            condition,
            update,
            body,
        })
    }

    fn block_body(&mut self) -> Result<Vec<Stmt>, ParseError> {
        let mut statements = Vec::new();
        loop {
            while self.peek() == &Token::Semicolon {
                self.index += 1;
            }
            if self.peek() == &Token::RightBrace {
                self.index += 1;
                break;
            }
            if self.peek() == &Token::Eof {
                return Err(ParseError::UnexpectedEnd);
            }
            statements.push(self.statement()?);
        }
        Ok(statements)
    }
}

pub fn compile(expression: &Expr) -> Result<Compiled, CompileError> {
    let mut compiler = Compiler::new();
    compiler.expression(expression)?;
    compiler.emit(Instruction::return_());
    Ok(compiler.finish())
}

/// Compiles a program. Statements produce no value; the final statement's
/// value is kept only when it is an expression statement (REPL semantics),
/// otherwise the program evaluates to `undefined`.
pub fn compile_program(program: &[Stmt]) -> Result<Compiled, CompileError> {
    let mut compiler = Compiler::new();
    let last_keeps_value = matches!(program.last(), Some(Stmt::Expression(_)));
    for (index, statement) in program.iter().enumerate() {
        let wants_value = last_keeps_value && index + 1 == program.len();
        compiler.statement(statement, wants_value)?;
    }
    if !last_keeps_value {
        compiler.emit(Instruction::load_undefined());
    }
    compiler.emit(Instruction::return_());
    Ok(compiler.finish())
}

struct Local {
    slot: usize,
    is_const: bool,
}

struct Compiler {
    instructions: Vec<Instruction>,
    /// String-constant pool; `OP_LOAD_STRING` operands index into it.
    pool: Vec<Value>,
    string_indices: HashMap<String, usize>,
    scopes: Vec<HashMap<String, Local>>,
    locals: usize,
}

impl Compiler {
    fn new() -> Self {
        Self {
            instructions: Vec::new(),
            pool: Vec::new(),
            string_indices: HashMap::new(),
            scopes: vec![HashMap::new()],
            locals: 0,
        }
    }

    fn finish(self) -> Compiled {
        Compiled {
            instructions: self.instructions,
            pool: self.pool,
        }
    }

    fn emit(&mut self, instruction: Instruction) -> usize {
        self.instructions.push(instruction);
        self.instructions.len() - 1
    }

    fn patch(&mut self, index: usize, target: usize) {
        self.instructions[index].operand = target as i32;
    }

    fn string_index(&mut self, text: &str) -> usize {
        if let Some(&index) = self.string_indices.get(text) {
            return index;
        }
        let index = self.pool.len();
        self.pool.push(Value::string_literal(text));
        self.string_indices.insert(text.to_string(), index);
        index
    }

    fn declare(&mut self, name: &str, is_const: bool) -> Result<usize, CompileError> {
        let slot = self.locals;
        if slot >= LOCAL_COUNT {
            return Err(CompileError::TooManyLocals);
        }
        self.locals += 1;
        self.scopes
            .last_mut()
            .expect("a scope is always open")
            .insert(name.to_string(), Local { slot, is_const });
        Ok(slot)
    }

    fn lookup(&self, name: &str) -> Option<&Local> {
        for scope in self.scopes.iter().rev() {
            if let Some(local) = scope.get(name) {
                return Some(local);
            }
        }
        None
    }

    fn expression(&mut self, expression: &Expr) -> Result<(), CompileError> {
        match expression {
            Expr::Int(value) => {
                if *value >= i32::MIN as i64 && *value <= i32::MAX as i64 {
                    self.emit(Instruction::load_int(*value));
                } else {
                    // Integers beyond `i32` go through the float path: exact
                    // up to 2^53, matching JS double semantics.
                    self.instructions
                        .extend(Instruction::load_f64(*value as f64));
                }
            }
            Expr::Float(value) => self.instructions.extend(Instruction::load_f64(*value)),
            Expr::Bool(value) => {
                self.emit(Instruction::load_bool(*value));
            }
            Expr::Null => {
                self.emit(Instruction::load_null());
            }
            Expr::Undefined => {
                self.emit(Instruction::load_undefined());
            }
            Expr::Var(name) => {
                let slot = self
                    .lookup(name)
                    .ok_or_else(|| CompileError::Undeclared(name.clone()))?
                    .slot;
                self.emit(Instruction::get_local(slot));
            }
            Expr::String(text) => {
                let index = self.string_index(text);
                self.emit(Instruction::load_string(index));
            }
            Expr::Template(parts) => {
                // Every substitution is explicitly stringified so `${a}${b}`
                // never performs a numeric add, and a trailing text part
                // (possibly empty) keeps the result a string, as in JS.
                for (index, part) in parts.iter().enumerate() {
                    match part {
                        TemplatePart::Text(text) => {
                            let pool_index = self.string_index(text);
                            self.emit(Instruction::load_string(pool_index));
                        }
                        TemplatePart::Substitution(substitution) => {
                            self.expression(substitution)?;
                            self.emit(Instruction::unary(r8_abi::OP_TO_STRING));
                        }
                    }
                    if index > 0 {
                        self.emit(Instruction::binary(r8_abi::OP_ADD));
                    }
                }
                if parts.is_empty() {
                    // `` evaluates to the empty string.
                    let pool_index = self.string_index("");
                    self.emit(Instruction::load_string(pool_index));
                } else if !matches!(parts.last(), Some(TemplatePart::Text(_))) {
                    // Load the trailing empty string, then concatenate: the
                    // ADD comes after its second operand is on the stack.
                    let pool_index = self.string_index("");
                    self.emit(Instruction::load_string(pool_index));
                    self.emit(Instruction::binary(r8_abi::OP_ADD));
                }
            }
            Expr::Assign(op, name, value) => {
                let (slot, is_const) = match self.lookup(name) {
                    Some(local) => (local.slot, local.is_const),
                    None => return Err(CompileError::Undeclared(name.clone())),
                };
                if is_const {
                    return Err(CompileError::ConstAssignment(name.clone()));
                }
                if *op != AssignOp::Assign {
                    // Compound assignment reads the target first.
                    self.emit(Instruction::get_local(slot));
                }
                self.expression(value)?;
                let opcode = match op {
                    AssignOp::Assign => None,
                    AssignOp::AddAssign => BinaryOp::Add.opcode(),
                    AssignOp::SubAssign => BinaryOp::Sub.opcode(),
                    AssignOp::MulAssign => BinaryOp::Mul.opcode(),
                    AssignOp::DivAssign => BinaryOp::Div.opcode(),
                    AssignOp::ModAssign => BinaryOp::Mod.opcode(),
                };
                if let Some(opcode) = opcode {
                    self.emit(Instruction::binary(opcode));
                }
                // The assignment expression's value is the assigned value.
                self.emit(Instruction::dup());
                self.emit(Instruction::set_local(slot));
            }
            Expr::Binary(op, left, right) => match op {
                BinaryOp::LogicalAnd | BinaryOp::LogicalOr => {
                    // Short-circuit: the deciding value is the result.
                    //   a && b: eval a; dup; jmp-if-false end; pop; eval b; end
                    //   a || b: eval a; dup; jmp-if-true end; pop; eval b; end
                    self.expression(left)?;
                    self.emit(Instruction::dup());
                    let jump = self.emit(if *op == BinaryOp::LogicalAnd {
                        Instruction::jmp_if_false(0)
                    } else {
                        Instruction::jmp_if_true(0)
                    });
                    self.emit(Instruction::pop());
                    self.expression(right)?;
                    let end = self.instructions.len();
                    self.patch(jump, end);
                }
                _ => {
                    self.expression(left)?;
                    self.expression(right)?;
                    self.emit(Instruction::binary(op.opcode().expect("non-logical op")));
                }
            },
            Expr::Unary(op, operand) => {
                self.expression(operand)?;
                match op {
                    UnaryOp::Neg => {
                        self.emit(Instruction::unary(r8_abi::OP_NEG));
                    }
                    UnaryOp::Not => {
                        self.emit(Instruction::unary(r8_abi::OP_NOT));
                    }
                    // Unary plus is numeric coercion. Current operand types
                    // are numbers except booleans (`+true` should be 1 — TODO
                    // with numeric coercion ops).
                    UnaryOp::Plus => {}
                }
            }
            Expr::ConsoleLog(argument) => {
                self.expression(argument)?;
                self.emit(Instruction::print());
            }
        }
        Ok(())
    }

    fn statement(&mut self, statement: &Stmt, wants_value: bool) -> Result<(), CompileError> {
        match statement {
            Stmt::Expression(expression) => {
                self.expression(expression)?;
                if !wants_value {
                    self.emit(Instruction::pop());
                }
            }
            Stmt::Empty => {}
            Stmt::Let(declarators) | Stmt::Const(declarators) => {
                let is_const = matches!(statement, Stmt::Const(_));
                for declarator in declarators {
                    // The initializer compiles before the binding exists, so
                    // `let x = x` fails at compile time (a TDZ approximation).
                    if let Some(init) = &declarator.init {
                        self.expression(init)?;
                    } else {
                        self.emit(Instruction::load_undefined());
                    }
                    let slot = self.declare(&declarator.name, is_const)?;
                    self.emit(Instruction::set_local(slot));
                }
            }
            Stmt::Block(statements) => {
                self.scopes.push(HashMap::new());
                for statement in statements {
                    self.statement(statement, false)?;
                }
                self.scopes.pop();
            }
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => {
                self.expression(condition)?;
                let else_jump = self.emit(Instruction::jmp_if_false(0));
                self.statement(then_body, false)?;
                match else_body {
                    Some(else_body) => {
                        let end_jump = self.emit(Instruction::jmp(0));
                        let else_start = self.instructions.len();
                        self.patch(else_jump, else_start);
                        self.statement(else_body, false)?;
                        let end = self.instructions.len();
                        self.patch(end_jump, end);
                    }
                    None => {
                        let end = self.instructions.len();
                        self.patch(else_jump, end);
                    }
                }
            }
            Stmt::While { condition, body } => {
                let loop_start = self.instructions.len();
                self.expression(condition)?;
                let end_jump = self.emit(Instruction::jmp_if_false(0));
                self.statement(body, false)?;
                self.emit(Instruction::jmp(loop_start));
                let end = self.instructions.len();
                self.patch(end_jump, end);
            }
            Stmt::For {
                init,
                condition,
                update,
                body,
            } => {
                // The whole `for` statement owns one scope for its init
                // declarations; per-iteration bindings arrive with closures.
                self.scopes.push(HashMap::new());
                if let Some(init) = init {
                    self.statement(init, false)?;
                }
                let loop_start = self.instructions.len();
                match condition {
                    Some(condition) => self.expression(condition)?,
                    None => {
                        self.emit(Instruction::load_bool(true));
                    }
                }
                let end_jump = self.emit(Instruction::jmp_if_false(0));
                self.statement(body, false)?;
                if let Some(update) = update {
                    self.expression(update)?;
                    self.emit(Instruction::pop());
                }
                self.emit(Instruction::jmp(loop_start));
                let end = self.instructions.len();
                self.patch(end_jump, end);
                self.scopes.pop();
            }
        }
        Ok(())
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
        let compiled = compile(&parse("1+1").unwrap()).unwrap();
        assert_eq!(
            compiled.instructions,
            vec![
                Instruction::load_int(1),
                Instruction::load_int(1),
                Instruction::binary(OP_ADD),
                Instruction::return_()
            ]
        );
        assert!(compiled.pool.is_empty());
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
    fn lexes_assignment_operators() {
        assert_eq!(
            tokenize("a = b += c -= d *= e /= f %= g").unwrap(),
            vec![
                Token::Identifier("a".into()),
                Token::EqAssign,
                Token::Identifier("b".into()),
                Token::PlusEq,
                Token::Identifier("c".into()),
                Token::MinusEq,
                Token::Identifier("d".into()),
                Token::StarEq,
                Token::Identifier("e".into()),
                Token::SlashEq,
                Token::Identifier("f".into()),
                Token::PercentEq,
                Token::Identifier("g".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn lexes_keywords_and_braces() {
        assert_eq!(
            tokenize("let var const if else while for { }").unwrap(),
            vec![
                Token::Let,
                Token::Var,
                Token::Const,
                Token::If,
                Token::Else,
                Token::While,
                Token::For,
                Token::LeftBrace,
                Token::RightBrace,
                Token::Eof
            ]
        );
    }

    #[test]
    fn lexes_string_literals() {
        assert_eq!(
            tokenize("'hi' \"there\"").unwrap(),
            vec![
                Token::String("hi".into()),
                Token::String("there".into()),
                Token::Eof
            ]
        );
        // Escapes cook into their characters.
        assert_eq!(
            tokenize(r#"'a\nb\t\\\'\"\0'"#).unwrap(),
            vec![Token::String("a\nb\t\\'\"\0".into()), Token::Eof]
        );
        assert_eq!(
            tokenize(r#""\x41\u{48}""#).unwrap(),
            vec![Token::String("AH".into()), Token::Eof]
        );
        // Identity escapes pass the character through.
        assert_eq!(
            tokenize(r#"'A'"#).unwrap(),
            vec![Token::String("A".into()), Token::Eof]
        );
    }

    #[test]
    fn rejects_bad_strings() {
        assert_eq!(parse("'oops"), Err(ParseError::UnterminatedString));
        assert_eq!(parse("'line\nbreak'"), Err(ParseError::UnterminatedString));
        assert_eq!(parse(r"'\u{zz}'"), Err(ParseError::InvalidEscape));
        assert_eq!(parse(r"'\u12'"), Err(ParseError::InvalidEscape));
    }

    #[test]
    fn lexes_template_literals() {
        assert_eq!(
            tokenize("`hello ${name}!`").unwrap(),
            vec![
                Token::TemplateStart,
                Token::TemplateText("hello ".into()),
                Token::TemplateExprOpen,
                Token::Identifier("name".into()),
                Token::TemplateExprClose,
                Token::TemplateText("!".into()),
                Token::TemplateEnd,
                Token::Eof
            ]
        );
        // Nested templates inside substitutions lex structurally: the inner
        // backtick opens a nested TemplateStart/TemplateEnd pair, and braces
        // inside a substitution are tracked so `}` closes the substitution
        // only at depth zero. (Object literals with `:` come in Phase 5.)
        let tokens = tokenize("`a${ {} }c`").unwrap();
        assert_eq!(
            tokens
                .iter()
                .filter(|t| matches!(t, Token::TemplateStart))
                .count(),
            1
        );
        assert!(tokens.contains(&Token::LeftBrace));
        assert!(tokens.contains(&Token::RightBrace));
        assert_eq!(
            tokens
                .iter()
                .filter(|t| matches!(t, Token::TemplateExprClose))
                .count(),
            1
        );
        // A nested template produces two TemplateStart/End pairs.
        let tokens = tokenize("`${ `x${1}` }`").unwrap();
        assert_eq!(
            tokens
                .iter()
                .filter(|t| matches!(t, Token::TemplateStart))
                .count(),
            2
        );
        assert_eq!(
            tokens
                .iter()
                .filter(|t| matches!(t, Token::TemplateEnd))
                .count(),
            2
        );
    }

    #[test]
    fn parses_templates() {
        assert_eq!(
            parse("`hello ${name}!`").unwrap(),
            Expr::Template(vec![
                TemplatePart::Text("hello ".into()),
                TemplatePart::Substitution(Expr::Var("name".into())),
                TemplatePart::Text("!".into()),
            ])
        );
        // Substitutions hold full expressions.
        assert_eq!(
            parse("`x${1 + 1}y`").unwrap(),
            Expr::Template(vec![
                TemplatePart::Text("x".into()),
                TemplatePart::Substitution(Expr::Binary(
                    BinaryOp::Add,
                    Box::new(Expr::Int(1)),
                    Box::new(Expr::Int(1))
                )),
                TemplatePart::Text("y".into()),
            ])
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
    fn parses_variables_and_assignment() {
        assert_eq!(parse("x").unwrap(), Expr::Var("x".into()));
        assert_eq!(
            parse("x = 5").unwrap(),
            Expr::Assign(AssignOp::Assign, "x".into(), Box::new(Expr::Int(5)))
        );
        // Assignment is right-associative and lowest precedence.
        assert_eq!(
            parse("x = y = 5").unwrap(),
            Expr::Assign(
                AssignOp::Assign,
                "x".into(),
                Box::new(Expr::Assign(
                    AssignOp::Assign,
                    "y".into(),
                    Box::new(Expr::Int(5))
                ))
            )
        );
        assert_eq!(
            parse("x += 1").unwrap(),
            Expr::Assign(AssignOp::AddAssign, "x".into(), Box::new(Expr::Int(1)))
        );
        // Addition binds tighter than assignment.
        assert_eq!(
            parse("x = 1 + 2").unwrap(),
            Expr::Assign(
                AssignOp::Assign,
                "x".into(),
                Box::new(Expr::Binary(
                    BinaryOp::Add,
                    Box::new(Expr::Int(1)),
                    Box::new(Expr::Int(2))
                ))
            )
        );
    }

    #[test]
    fn rejects_invalid_assignment_targets() {
        assert_eq!(
            parse("1 = 2").unwrap_err(),
            ParseError::Unsupported("only simple assignment targets")
        );
        assert_eq!(
            parse("console.log(1) = 2").unwrap_err(),
            ParseError::Unsupported("only simple assignment targets")
        );
    }

    #[test]
    fn parses_declarations() {
        let program = parse_program("let x; let y = 2; var z = 3; const w = 4;").unwrap();
        assert_eq!(program.len(), 4);
        assert_eq!(
            program[0],
            Stmt::Let(vec![Declarator {
                name: "x".into(),
                init: None
            }])
        );
        assert_eq!(
            program[3],
            Stmt::Const(vec![Declarator {
                name: "w".into(),
                init: Some(Expr::Int(4))
            }])
        );
    }

    #[test]
    fn parses_multiple_declarators() {
        let program = parse_program("let a = 1, b = 2, c;").unwrap();
        assert_eq!(program.len(), 1);
        assert_eq!(
            program[0],
            Stmt::Let(vec![
                Declarator {
                    name: "a".into(),
                    init: Some(Expr::Int(1))
                },
                Declarator {
                    name: "b".into(),
                    init: Some(Expr::Int(2))
                },
                Declarator {
                    name: "c".into(),
                    init: None
                },
            ])
        );
    }

    #[test]
    fn rejects_const_without_initializer() {
        assert_eq!(
            parse_program("const x;").unwrap_err(),
            ParseError::MissingInitializer
        );
    }

    #[test]
    fn rejects_keyword_as_declarator_name() {
        assert_eq!(
            parse_program("let if = 1;").unwrap_err(),
            ParseError::ExpectedIdentifier
        );
    }

    #[test]
    fn parses_if_else() {
        let program =
            parse_program("if (a < b) { x = 1; } else if (a == b) { x = 2; } else { x = 3; }")
                .unwrap();
        assert_eq!(program.len(), 1);
        assert!(matches!(program[0], Stmt::If { .. }));
        // The else branch is itself an `if` (else-if chains).
        if let Stmt::If {
            else_body: Some(else_body),
            ..
        } = &program[0]
        {
            assert!(matches!(else_body.as_ref(), Stmt::If { .. }));
        } else {
            panic!("expected an if statement");
        }
    }

    #[test]
    fn parses_while_and_for() {
        assert!(matches!(
            parse_program("while (i < 3) i += 1;").unwrap()[0],
            Stmt::While { .. }
        ));
        let program = parse_program("for (let i = 0; i < 3; i += 1) { total += i; }").unwrap();
        assert!(matches!(program[0], Stmt::For { .. }));
        assert!(matches!(
            parse_program("for (;;) { 1; }").unwrap()[0],
            Stmt::For { .. }
        ));
    }

    #[test]
    fn rejects_for_of_and_for_in() {
        // Bracket-free sources: `[` would fail in the lexer first.
        assert_eq!(
            parse_program("for (const x of 1) { }").unwrap_err(),
            ParseError::Unsupported("for-of and for-in loops")
        );
        assert_eq!(
            parse_program("for (let x in 1) { }").unwrap_err(),
            ParseError::Unsupported("for-of and for-in loops")
        );
    }

    #[test]
    fn rejects_too_many_locals() {
        let source: String = (0..300)
            .map(|i| format!("let x{i} = 1;"))
            .collect::<Vec<_>>()
            .join(" ");
        let program = parse_program(&source).unwrap();
        assert_eq!(
            compile_program(&program).unwrap_err(),
            CompileError::TooManyLocals
        );
    }

    #[test]
    fn parses_program_statements() {
        let program = parse_program("console.log(1 + 1);\nconsole.log(2 + 3 + 4);").unwrap();
        assert_eq!(program.len(), 2);

        assert!(parse_program("").unwrap().is_empty());
        assert!(parse_program("1;").unwrap().len() == 1);
        assert!(parse_program(";; 1 ;;").unwrap().len() == 1);
        // No ASI: statements need separators unless followed by } or EOF.
        assert_eq!(parse_program("1 2").unwrap_err(), ParseError::TrailingInput);
        assert_eq!(parse_program("{ let x = 1; }").unwrap().len(), 1);
    }

    #[test]
    fn compiles_float_literal_to_two_instructions() {
        let compiled = compile(&parse("1.5").unwrap()).unwrap();
        assert_eq!(compiled.instructions.len(), 3);
        assert_eq!(compiled.instructions[0].op, r8_abi::OP_LOAD_F64_HI);
        assert_eq!(compiled.instructions[1].op, r8_abi::OP_LOAD_F64);
        assert_eq!(compiled.instructions[2].op, OP_RETURN);
    }

    #[test]
    fn compiles_large_integers_through_the_float_path() {
        let compiled = compile(&parse("5000000000").unwrap()).unwrap();
        assert_eq!(compiled.instructions[0].op, r8_abi::OP_LOAD_F64_HI);
    }

    #[test]
    fn compiles_string_literals_into_the_pool() {
        let compiled = compile(&parse("'hello'").unwrap()).unwrap();
        assert_eq!(compiled.instructions.len(), 2);
        assert_eq!(compiled.instructions[0].op, r8_abi::OP_LOAD_STRING);
        assert_eq!(compiled.instructions[0].operand, 0);
        assert_eq!(compiled.pool.len(), 1);
        assert_eq!(compiled.pool[0].as_str(), Some("hello"));
    }

    #[test]
    fn interns_duplicate_string_literals() {
        let compiled = compile(&parse("'a' + 'b' + 'a'").unwrap()).unwrap();
        assert_eq!(compiled.pool.len(), 2);
        let operands: Vec<i32> = compiled
            .instructions
            .iter()
            .filter(|i| i.op == r8_abi::OP_LOAD_STRING)
            .map(|i| i.operand)
            .collect();
        assert_eq!(operands, vec![0, 1, 0]);
    }

    #[test]
    fn compiles_templates_with_to_string() {
        // `x${1}y` -> load "x"; load 1; toString; add; load "y"; add
        let compiled = compile(&parse("`x${1}y`").unwrap()).unwrap();
        let ops: Vec<u32> = compiled.instructions.iter().map(|i| i.op).collect();
        assert_eq!(
            ops,
            vec![
                r8_abi::OP_LOAD_STRING, // "x"
                r8_abi::OP_LOAD_INT,    // 1
                r8_abi::OP_TO_STRING,   // stringify
                r8_abi::OP_ADD,         // "x" + "1"
                r8_abi::OP_LOAD_STRING, // "y"
                r8_abi::OP_ADD,         // ... + "y"
                OP_RETURN
            ]
        );
        // Pool holds just the two text chunks ("x" and "y"); the trailing
        // empty string only appears when the template ends on a
        // substitution.
        assert_eq!(compiled.pool.len(), 2);
        assert_eq!(compiled.pool[1].as_str(), Some("y"));
    }

    #[test]
    fn compiles_substitution_final_templates_with_trailing_empty() {
        // `${1}` -> toString(1) + ""
        let compiled = compile(&parse("`${1}`").unwrap()).unwrap();
        let ops: Vec<u32> = compiled.instructions.iter().map(|i| i.op).collect();
        assert_eq!(
            ops,
            vec![
                r8_abi::OP_LOAD_INT,    // 1
                r8_abi::OP_TO_STRING,   // stringify
                r8_abi::OP_LOAD_STRING, // ""
                r8_abi::OP_ADD,         // "1" + ""
                OP_RETURN
            ]
        );
        assert_eq!(compiled.pool.len(), 1);
        assert_eq!(compiled.pool[0].as_str(), Some(""));
    }

    #[test]
    fn compiles_empty_template() {
        let compiled = compile(&parse("``").unwrap()).unwrap();
        assert_eq!(compiled.instructions.len(), 2);
        assert_eq!(compiled.instructions[0].op, r8_abi::OP_LOAD_STRING);
        assert_eq!(compiled.pool.len(), 1);
        assert_eq!(compiled.pool[0].as_str(), Some(""));
    }

    #[test]
    fn compiles_variables_to_local_slots() {
        let program = parse_program("let x = 1; let y = x + 1; y").unwrap();
        let compiled = compile_program(&program).unwrap();
        assert_eq!(
            compiled.instructions,
            vec![
                Instruction::load_int(1),
                Instruction::set_local(0),
                Instruction::get_local(0),
                Instruction::load_int(1),
                Instruction::binary(OP_ADD),
                Instruction::set_local(1),
                Instruction::get_local(1),
                Instruction::return_()
            ]
        );
    }

    #[test]
    fn compiles_shadowing_to_distinct_slots() {
        let program = parse_program("let x = 1; { let x = 2; x; } x").unwrap();
        let compiled = compile_program(&program).unwrap();
        let get_slots: Vec<i32> = compiled
            .instructions
            .iter()
            .filter(|i| i.op == r8_abi::OP_GET_LOCAL)
            .map(|i| i.operand)
            .collect();
        // Inner `x` is slot 1, outer `x` is slot 0.
        assert_eq!(get_slots, vec![1, 0]);
    }

    #[test]
    fn compiles_assignment_with_value() {
        let program = parse_program("let x = 0; let y = (x = 5) + 1; y").unwrap();
        let compiled = compile_program(&program).unwrap();
        assert_eq!(
            compiled.instructions,
            vec![
                Instruction::load_int(0),
                Instruction::set_local(0),
                // (x = 5) + 1
                Instruction::load_int(5),
                Instruction::dup(),
                Instruction::set_local(0),
                Instruction::load_int(1),
                Instruction::binary(OP_ADD),
                Instruction::set_local(1),
                Instruction::get_local(1),
                Instruction::return_()
            ]
        );
    }

    #[test]
    fn reports_compile_errors() {
        assert_eq!(
            compile(&parse("x").unwrap()).unwrap_err(),
            CompileError::Undeclared("x".into())
        );
        let program = parse_program("const x = 1; x = 2;").unwrap();
        assert_eq!(
            compile_program(&program).unwrap_err(),
            CompileError::ConstAssignment("x".into())
        );
        // TDZ approximation: the initializer cannot see its own binding.
        let program = parse_program("let x = x;").unwrap();
        assert_eq!(
            compile_program(&program).unwrap_err(),
            CompileError::Undeclared("x".into())
        );
    }

    #[test]
    fn compiles_if_else_jumps() {
        let program =
            parse_program("let x = 0; if (1 < 2) { x = 10; } else { x = 20; } x").unwrap();
        let compiled = compile_program(&program).unwrap();
        let instructions = &compiled.instructions;
        // Locate the conditional jump and verify its patched targets.
        let branch = instructions
            .iter()
            .position(|i| i.op == r8_abi::OP_JMP_IF_FALSE)
            .unwrap();
        let unconditional = instructions[branch + 1..]
            .iter()
            .position(|i| i.op == r8_abi::OP_JMP)
            .unwrap()
            + branch
            + 1;
        // The conditional jump lands on the else body; the unconditional
        // jump lands after it.
        assert_eq!(instructions[branch].operand as usize, unconditional + 1);
        assert_eq!(
            instructions[unconditional].operand as usize,
            instructions.len() - 2 // before `get_local` + `return`
        );
    }

    #[test]
    fn compiles_while_loop() {
        let program = parse_program("let i = 0; while (i < 3) { i += 1; } i").unwrap();
        let compiled = compile_program(&program).unwrap();
        let back_jump = compiled
            .instructions
            .iter()
            .rev()
            .find(|i| i.op == r8_abi::OP_JMP)
            .unwrap();
        // The loop's back-jump lands on the condition load.
        let condition_index = back_jump.operand as usize;
        assert_eq!(
            compiled.instructions[condition_index].op,
            r8_abi::OP_GET_LOCAL
        );
    }

    #[test]
    fn compiles_for_loop() {
        let program =
            parse_program("let total = 0; for (let i = 0; i < 3; i += 1) { total += i; } total")
                .unwrap();
        let compiled = compile_program(&program).unwrap();
        // for-init declares i in slot 1.
        assert!(
            compiled
                .instructions
                .iter()
                .any(|i| i.op == r8_abi::OP_SET_LOCAL && i.operand == 1)
        );
        // The loop contains a back-jump to a condition and an update POP.
        assert!(
            compiled
                .instructions
                .iter()
                .rev()
                .any(|i| i.op == r8_abi::OP_JMP)
        );
        assert!(
            compiled
                .instructions
                .iter()
                .any(|i| i.op == r8_abi::OP_JMP_IF_FALSE)
        );
    }

    #[test]
    fn compiles_logical_operators_with_dup_and_jumps() {
        let program = parse_program("let x = 0; x = (0 && 5) || 7; x").unwrap();
        let compiled = compile_program(&program).unwrap();
        // One DUP per logical operator plus the assignment's value DUP.
        assert_eq!(
            compiled
                .instructions
                .iter()
                .filter(|i| i.op == r8_abi::OP_DUP)
                .count(),
            3
        );
        assert_eq!(
            compiled
                .instructions
                .iter()
                .filter(|i| i.op == r8_abi::OP_JMP_IF_FALSE)
                .count(),
            1
        );
        assert_eq!(
            compiled
                .instructions
                .iter()
                .filter(|i| i.op == r8_abi::OP_JMP_IF_TRUE)
                .count(),
            1
        );
    }

    #[test]
    fn keeps_only_final_expression_value() {
        // Non-final statements pop; a non-expression final statement yields
        // undefined.
        let program = parse_program("let a = 1;").unwrap();
        let compiled = compile_program(&program).unwrap();
        assert_eq!(
            compiled.instructions,
            vec![
                Instruction::load_int(1),
                Instruction::set_local(0),
                Instruction::load_undefined(),
                Instruction::return_()
            ]
        );
        let program = parse_program("1; 2").unwrap();
        let compiled = compile_program(&program).unwrap();
        assert_eq!(
            compiled.instructions,
            vec![
                Instruction::load_int(1),
                Instruction::pop(),
                Instruction::load_int(2),
                Instruction::return_()
            ]
        );
        assert_eq!(
            compile_program(&[]).unwrap().instructions,
            vec![Instruction::load_undefined(), Instruction::return_()]
        );
    }

    #[test]
    fn compiles_console_log_to_print() {
        let compiled = compile(&parse("console.log(2+3)").unwrap()).unwrap();
        assert_eq!(
            compiled.instructions,
            vec![
                Instruction::load_int(2),
                Instruction::load_int(3),
                Instruction::binary(OP_ADD),
                Instruction::print(),
                Instruction::return_()
            ]
        );
        assert_eq!(compiled.instructions[3].op, r8_abi::OP_PRINT);
    }
}
