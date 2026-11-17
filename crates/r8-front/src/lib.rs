//! R8 front end: lexer, parser, AST, and bytecode compiler.
//!
//! Phase 4 adds functions: declarations, anonymous function expressions,
//! arrow functions (with paren-free single parameters), `return`, calls, and
//! JS-correct closures. Each function compiles into its own instruction
//! buffer; buffers concatenate into the flat stream in completion order and
//! the machine finds them through a `KeelFunction` table. Captured bindings
//! are detected by a syntactic prescan (shadowing-aware) and compiled into
//! heap cells so closures share mutation; upvalue chains are built
//! transitively at reference resolution.
//!
//! Phase 2 compiled statements: `let`/`const`/`var` declarations, assignment,
//! blocks, `if`/`else`, `while`, `for`, and short-circuit `&&`/`||`, with a
//! scope-aware compiler that gives every declaration its own local slot and
//! patches absolute-index jumps.

use r8_abi::{Instruction, KeelFunction, Value};
use std::collections::{HashMap, HashSet};
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

/// An arrow body: either a bare expression (its value is the return value)
/// or a braced block of statements.
#[derive(Debug, Clone, PartialEq)]
pub enum ArrowBody {
    Expression(Box<Expr>),
    Block(Vec<Stmt>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TemplatePart {
    Text(String),
    Substitution(Expr),
}

/// An object literal property's key.
#[derive(Debug, Clone, PartialEq)]
pub enum ObjectKey {
    /// `{ name: ... }` and shorthand `{ name }`.
    Ident(String),
    /// `{ "name": ... }`.
    Literal(String),
    /// `{ [expr]: ... }` — the expression is stringified.
    Computed(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ObjectProperty {
    pub key: ObjectKey,
    /// `None` for shorthand properties (value = a reference to the key).
    pub value: Option<Expr>,
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
    /// Anonymous `function (params) { body }` expression.
    FunctionExpr {
        params: Vec<String>,
        body: Vec<Stmt>,
    },
    /// `(params) => body` or `x => body` (no `this`/`arguments` binding in
    /// this phase, so arrow bodies compile like plain functions).
    Arrow {
        params: Vec<String>,
        body: ArrowBody,
    },
    /// `callee(arg, ...)` — the callee is any expression.
    Call(Box<Expr>, Vec<Expr>),
    /// `target = value` / `target op= value`, where the target is a variable,
    /// a property, or a computed index (guaranteed by the parser).
    Assign(AssignOp, Box<Expr>, Box<Expr>),
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    Unary(UnaryOp, Box<Expr>),
    /// The `console.log` intrinsic, recognized syntactically until a general
    /// call mechanism exists (Phase 4).
    ConsoleLog(Box<Expr>),
    /// `[a, b, ...]` — holes (`[1,,2]`) carry `None`.
    Array(Vec<Option<Expr>>),
    /// `{ key: value, ... }`.
    Object(Vec<ObjectProperty>),
    /// `receiver.name` (compiles to an index read with a string key).
    Property(Box<Expr>, String),
    /// `receiver[key]`.
    Index(Box<Expr>, Box<Expr>),
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
    Function {
        name: String,
        params: Vec<String>,
        body: Vec<Stmt>,
    },
    Return(Option<Expr>),
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
    LeftBracket,
    RightBracket,
    Dot,
    Colon,
    Comma,
    Semicolon,
    EqArrow,
    Identifier(String),
    Let,
    Var,
    Const,
    If,
    Else,
    While,
    For,
    Function,
    Return,
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

/// Bytecode plus its string-constant pool and function table; all three are
/// passed to the machine.
#[derive(Debug, Clone)]
pub struct Compiled {
    pub instructions: Vec<Instruction>,
    pub pool: Vec<Value>,
    pub functions: Vec<KeelFunction>,
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
                } else if self.characters.get(self.index + 1) == Some(&'>') {
                    self.tokens.push(Token::EqArrow);
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
            '[' => {
                self.tokens.push(Token::LeftBracket);
                self.index += 1;
            }
            ']' => {
                self.tokens.push(Token::RightBracket);
                self.index += 1;
            }
            ':' => {
                self.tokens.push(Token::Colon);
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
                    "function" => Token::Function,
                    "return" => Token::Return,
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
        match left {
            Expr::Var(_) | Expr::Property(_, _) | Expr::Index(_, _) => {}
            _ => return Err(ParseError::Unsupported("only simple assignment targets")),
        }
        let value = self.assignment()?;
        Ok(Expr::Assign(op, Box::new(left), Box::new(value)))
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
            _ => return self.postfix(),
        };
        self.index += 1;
        let operand = self.unary()?;
        Ok(Expr::Unary(op, Box::new(operand)))
    }

    /// Postfix operators: call arguments `f(a, b)`, property access `a.b`,
    /// and computed access `a[key]`.
    fn postfix(&mut self) -> Result<Expr, ParseError> {
        let mut expression = self.primary()?;
        loop {
            match self.peek() {
                Token::LParen => {
                    self.index += 1;
                    let mut arguments = Vec::new();
                    if self.peek() == &Token::RParen {
                        self.index += 1;
                    } else {
                        loop {
                            arguments.push(self.assignment()?);
                            match self.next() {
                                Token::Comma => {}
                                Token::RParen => break,
                                Token::Eof => return Err(ParseError::UnexpectedEnd),
                                _ => return Err(ParseError::TrailingInput),
                            }
                        }
                    }
                    expression = Expr::Call(Box::new(expression), arguments);
                }
                Token::Dot => {
                    self.index += 1;
                    let name = self.property_name()?;
                    expression = Expr::Property(Box::new(expression), name);
                }
                Token::LeftBracket => {
                    self.index += 1;
                    let key = self.assignment()?;
                    match self.next() {
                        Token::RightBracket => {}
                        Token::Eof => return Err(ParseError::UnexpectedEnd),
                        _ => return Err(ParseError::TrailingInput),
                    }
                    expression = Expr::Index(Box::new(expression), Box::new(key));
                }
                _ => break,
            }
        }
        Ok(expression)
    }

    /// A property name after `.`: an identifier or a reserved word (JS
    /// allows `a.for`, `a.class`, ...).
    fn property_name(&mut self) -> Result<String, ParseError> {
        let name = match self.next() {
            Token::Identifier(name) => name,
            Token::Let => "let".into(),
            Token::Var => "var".into(),
            Token::Const => "const".into(),
            Token::If => "if".into(),
            Token::Else => "else".into(),
            Token::While => "while".into(),
            Token::For => "for".into(),
            Token::Function => "function".into(),
            Token::Return => "return".into(),
            Token::True => "true".into(),
            Token::False => "false".into(),
            Token::Null => "null".into(),
            Token::Undefined => "undefined".into(),
            Token::Eof => return Err(ParseError::UnexpectedEnd),
            _ => return Err(ParseError::TrailingInput),
        };
        Ok(name)
    }

    /// `[a, b, ...]` with holes (`[1,,2]`) and a trailing comma. The `[` was
    /// already consumed by `primary`.
    fn array_literal(&mut self) -> Result<Expr, ParseError> {
        let mut elements = Vec::new();
        loop {
            if self.peek() == &Token::RightBracket {
                self.index += 1;
                break;
            }
            if self.peek() == &Token::Eof {
                return Err(ParseError::UnexpectedEnd);
            }
            if self.peek() == &Token::Comma {
                self.index += 1;
                elements.push(None);
                continue;
            }
            elements.push(Some(self.assignment()?));
            match self.next() {
                Token::Comma => {}
                Token::RightBracket => break,
                Token::Eof => return Err(ParseError::UnexpectedEnd),
                _ => return Err(ParseError::TrailingInput),
            }
        }
        Ok(Expr::Array(elements))
    }

    /// `{ key: value, shorthand }` with string and computed keys and a
    /// trailing comma. In statement position `{` is a block; this parses
    /// only from expression position. The `{` was already consumed.
    fn object_literal(&mut self) -> Result<Expr, ParseError> {
        let mut properties = Vec::new();
        loop {
            while self.peek() == &Token::Comma {
                self.index += 1;
            }
            if self.peek() == &Token::RightBrace {
                self.index += 1;
                break;
            }
            if self.peek() == &Token::Eof {
                return Err(ParseError::UnexpectedEnd);
            }
            let key = match self.next() {
                Token::Identifier(name) => ObjectKey::Ident(name),
                Token::String(text) => ObjectKey::Literal(text),
                Token::LeftBracket => {
                    let expression = self.assignment()?;
                    match self.next() {
                        Token::RightBracket => {}
                        Token::Eof => return Err(ParseError::UnexpectedEnd),
                        _ => return Err(ParseError::TrailingInput),
                    }
                    ObjectKey::Computed(Box::new(expression))
                }
                Token::Eof => return Err(ParseError::UnexpectedEnd),
                _ => return Err(ParseError::Unsupported("object property keys")),
            };
            let value = if self.peek() == &Token::Colon {
                self.index += 1;
                Some(self.assignment()?)
            } else if matches!(key, ObjectKey::Ident(_)) {
                None // shorthand: value is a reference to the key
            } else {
                return Err(ParseError::TrailingInput);
            };
            properties.push(ObjectProperty { key, value });
            match self.next() {
                Token::Comma => {}
                Token::RightBrace => break,
                Token::Eof => return Err(ParseError::UnexpectedEnd),
                _ => return Err(ParseError::TrailingInput),
            }
        }
        Ok(Expr::Object(properties))
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
            Token::Identifier(name) => {
                if self.tokens.get(self.index) == Some(&Token::EqArrow) {
                    self.next(); // `=>`
                    let params = vec![name];
                    let body = self.arrow_body()?;
                    return Ok(Expr::Arrow { params, body });
                }
                Ok(Expr::Var(name))
            }
            Token::LeftBracket => self.array_literal(),
            Token::LeftBrace => self.object_literal(),
            Token::LParen => {
                // Arrow disambiguation: `(a, b) => ...` vs a parenthesized
                // expression. Lookahead scans for a simple identifier list
                // followed by `=>`. The `(` is already consumed here.
                if self.looks_like_arrow_params(self.index) {
                    let params = self.param_names()?;
                    self.next(); // `=>`
                    let body = self.arrow_body()?;
                    return Ok(Expr::Arrow { params, body });
                }
                let expression = self.expression()?;
                match self.next() {
                    Token::RParen => Ok(expression),
                    Token::Eof => Err(ParseError::UnexpectedEnd),
                    _ => Err(ParseError::TrailingInput),
                }
            }
            Token::Function => self.function_expression(),
            Token::Eof => Err(ParseError::UnexpectedEnd),
            _ => Err(ParseError::ExpectedExpression),
        }
    }

    /// From a token index right after `(`, decides whether the tokens form a
    /// plain parameter list followed by `=>` (without consuming anything).
    fn looks_like_arrow_params(&self, mut index: usize) -> bool {
        // Empty params: `() =>`.
        if matches!(self.tokens.get(index), Some(Token::RParen)) {
            return matches!(self.tokens.get(index + 1), Some(Token::EqArrow));
        }
        loop {
            if !matches!(self.tokens.get(index), Some(Token::Identifier(_))) {
                return false;
            }
            index += 1;
            match self.tokens.get(index) {
                Some(Token::Comma) => index += 1,
                Some(Token::RParen) => {
                    return matches!(self.tokens.get(index + 1), Some(Token::EqArrow));
                }
                _ => return false,
            }
        }
    }

    /// Parses the body after `=>`: a braced block or a bare expression.
    fn arrow_body(&mut self) -> Result<ArrowBody, ParseError> {
        if self.peek() == &Token::LeftBrace {
            Ok(ArrowBody::Block(self.function_body()?))
        } else {
            Ok(ArrowBody::Expression(Box::new(self.expression()?)))
        }
    }

    /// Parses `function (params) { body }` — anonymous function expressions
    /// only (named function expressions need self-binding machinery). The
    /// `function` keyword was already consumed by `primary`.
    fn function_expression(&mut self) -> Result<Expr, ParseError> {
        if let Token::Identifier(_) = self.peek() {
            return Err(ParseError::Unsupported("named function expressions"));
        }
        let params = self.param_list()?;
        let body = self.function_body()?;
        Ok(Expr::FunctionExpr { params, body })
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
            Token::Function => self.function_declaration(),
            Token::Return => self.return_statement(),
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

    fn function_declaration(&mut self) -> Result<Stmt, ParseError> {
        self.next(); // `function`
        let name = match self.next() {
            Token::Identifier(name) => name,
            Token::Eof => return Err(ParseError::UnexpectedEnd),
            _ => return Err(ParseError::ExpectedIdentifier),
        };
        let params = self.param_list()?;
        let body = self.function_body()?;
        Ok(Stmt::Function { name, params, body })
    }

    fn return_statement(&mut self) -> Result<Stmt, ParseError> {
        self.next(); // `return`
        // A return value is optional: `return;` ends the statement.
        let value = match self.peek() {
            Token::Semicolon | Token::RightBrace | Token::Eof => None,
            _ => Some(self.expression()?),
        };
        self.end_statement()?;
        Ok(Stmt::Return(value))
    }

    /// Parses `(a, b, c)` — identifiers separated by commas. Default values,
    /// rest parameters, and destructuring are later-phase work.
    fn param_list(&mut self) -> Result<Vec<String>, ParseError> {
        self.expect(&Token::LParen)?;
        self.param_names()
    }

    /// Parses the identifier list inside a parameter list; `(` must already
    /// be consumed.
    fn param_names(&mut self) -> Result<Vec<String>, ParseError> {
        let mut params = Vec::new();
        if self.peek() == &Token::RParen {
            self.index += 1;
            return Ok(params);
        }
        loop {
            match self.next() {
                Token::Identifier(name) => params.push(name),
                Token::Eof => return Err(ParseError::UnexpectedEnd),
                _ => return Err(ParseError::Unsupported("only simple parameter names")),
            }
            match self.next() {
                Token::Comma => {}
                Token::RParen => break,
                Token::EqAssign => return Err(ParseError::Unsupported("default parameters")),
                Token::Eof => return Err(ParseError::UnexpectedEnd),
                _ => return Err(ParseError::TrailingInput),
            }
        }
        Ok(params)
    }

    /// Parses a braced function body and returns its statements.
    fn function_body(&mut self) -> Result<Vec<Stmt>, ParseError> {
        self.expect(&Token::LeftBrace)?;
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
    let (_, _) = compiler.enter_function(&[], &[], Some(expression), false)?;
    Ok(compiler.finish())
}

/// Compiles a program. Statements produce no value; the final statement's
/// value is kept only when it is an expression statement (REPL semantics),
/// otherwise the program evaluates to `undefined`. The main body becomes
/// function-table entry 0, where the machine begins execution.
pub fn compile_program(program: &[Stmt]) -> Result<Compiled, CompileError> {
    let mut compiler = Compiler::new();
    compiler.enter_function(&[], program, None, true)?;
    Ok(compiler.finish())
}

/// One captured outer binding, as seen by the function being compiled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capture {
    /// A local slot in the immediately enclosing function.
    Local(usize),
    /// An upvalue of the immediately enclosing function (transitive).
    Upvalue(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resolved {
    Local { slot: usize, is_const: bool },
    Upvalue { index: usize, is_const: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Binding {
    slot: usize,
    is_const: bool,
}

struct Scope {
    bindings: HashMap<String, Binding>,
}

/// Per-function compilation state: its own instruction buffer, local slots,
/// upvalue captures, and scope-stack base.
struct FunctionState {
    index: usize,
    buffer: Vec<Instruction>,
    local_count: usize,
    captures: Vec<Capture>,
    upvalue_map: HashMap<String, (usize, bool)>,
    scopes_base: usize,
    /// Names (from the entry prescan) that nested functions capture; their
    /// declarations emit `OP_MAKE_CELL`.
    captured_names: HashSet<String>,
}

struct Compiler {
    /// String-constant pool; `OP_LOAD_STRING` operands index into it.
    pool: Vec<Value>,
    string_indices: HashMap<String, usize>,
    scopes: Vec<Scope>,
    /// The function table being built; indices are assigned when a function
    /// is entered, and layout fields are filled when it completes.
    functions: Vec<KeelFunction>,
    states: Vec<FunctionState>,
    completed: Vec<FunctionState>,
}

impl Compiler {
    fn new() -> Self {
        Self {
            pool: Vec::new(),
            string_indices: HashMap::new(),
            scopes: Vec::new(),
            functions: Vec::new(),
            states: Vec::new(),
            completed: Vec::new(),
        }
    }

    /// Concatenates completed function buffers into the flat instruction
    /// stream, rebasing absolute jumps, and assigns each function's
    /// `[start, start+len)` layout. Entry 0 (main) ends up last in the
    /// stream, which is fine: the machine starts at `functions[0].start`.
    fn finish(mut self) -> Compiled {
        let mut instructions = Vec::new();
        for mut state in self.completed.drain(..) {
            let start = instructions.len();
            for instruction in &mut state.buffer {
                if instruction.op == r8_abi::OP_JMP
                    || instruction.op == r8_abi::OP_JMP_IF_FALSE
                    || instruction.op == r8_abi::OP_JMP_IF_TRUE
                {
                    instruction.operand += start as i32;
                }
            }
            instructions.extend(state.buffer);
            self.functions[state.index].start = start as u32;
            self.functions[state.index].len = (instructions.len() - start) as u32;
        }
        Compiled {
            instructions,
            pool: self.pool,
            functions: self.functions,
        }
    }

    fn emit(&mut self, instruction: Instruction) -> usize {
        let state = self.states.last_mut().expect("compiling a function");
        state.buffer.push(instruction);
        state.buffer.len() - 1
    }

    fn patch(&mut self, index: usize, target: usize) {
        let state = self.states.last_mut().expect("compiling a function");
        state.buffer[index].operand = target as i32;
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
        let slot = self
            .states
            .last()
            .expect("compiling a function")
            .local_count;
        if slot >= LOCAL_COUNT {
            return Err(CompileError::TooManyLocals);
        }
        self.states
            .last_mut()
            .expect("compiling a function")
            .local_count += 1;
        self.scopes
            .last_mut()
            .expect("a scope is always open")
            .bindings
            .insert(name.to_string(), Binding { slot, is_const });
        Ok(slot)
    }

    fn local_bound(&self, name: &str) -> Option<Binding> {
        let scopes_base = self
            .states
            .last()
            .expect("compiling a function")
            .scopes_base;
        for scope in self.scopes[scopes_base..].iter().rev() {
            if let Some(&binding) = scope.bindings.get(name) {
                return Some(binding);
            }
        }
        None
    }

    /// Resolves `name` for the function being compiled: a local slot, or an
    /// upvalue (building the transitive capture chain through intermediate
    /// functions when needed).
    fn resolve(&mut self, name: &str) -> Option<Resolved> {
        if let Some(binding) = self.local_bound(name) {
            return Some(Resolved::Local {
                slot: binding.slot,
                is_const: binding.is_const,
            });
        }
        let current = self.states.last().expect("compiling a function");
        if let Some(&(index, is_const)) = current.upvalue_map.get(name) {
            return Some(Resolved::Upvalue { index, is_const });
        }
        // Find the owner scope among enclosing functions' scopes.
        let scopes_base = current.scopes_base;
        let mut owner = None;
        for (i, scope) in self.scopes[..scopes_base].iter().enumerate().rev() {
            if let Some(&binding) = scope.bindings.get(name) {
                owner = Some((i, binding.slot, binding.is_const));
                break;
            }
        }
        let (owner_scope, owner_slot, owner_is_const) = owner?;
        // The owner's function is the last active one containing that scope;
        // every function from just inside it down to the current one adds a
        // capture (transitively via upvalues).
        let owner_fn = self
            .states
            .iter()
            .rposition(|state| state.scopes_base <= owner_scope)?;
        let mut capture = Capture::Local(owner_slot);
        for j in (owner_fn + 1)..self.states.len() {
            let index = self.states[j].captures.len();
            self.states[j].captures.push(capture);
            capture = Capture::Upvalue(index);
        }
        let index = self
            .states
            .last()
            .expect("compiling a function")
            .captures
            .len()
            - 1;
        self.states
            .last_mut()
            .expect("compiling a function")
            .upvalue_map
            .insert(name.to_string(), (index, owner_is_const));
        Some(Resolved::Upvalue {
            index,
            is_const: owner_is_const,
        })
    }

    /// Enters a new function: assigns its table index, pushes its state,
    /// declares parameters (with cells for captured ones), compiles the
    /// body, and pops. Returns the table index and the capture list for the
    /// creation site to emit.
    fn enter_function(
        &mut self,
        params: &[String],
        body: &[Stmt],
        expr_body: Option<&Expr>,
        wants_last_value: bool,
    ) -> Result<(usize, Vec<Capture>), CompileError> {
        let index = self.functions.len();
        self.functions.push(KeelFunction {
            start: 0,
            len: 0,
            local_count: 0,
            arity: params.len() as u32,
        });
        self.scopes.push(Scope {
            bindings: HashMap::new(),
        });
        let captured_names = Self::collect_captures(params, body, expr_body);
        self.states.push(FunctionState {
            index,
            buffer: Vec::new(),
            local_count: 0,
            captures: Vec::new(),
            upvalue_map: HashMap::new(),
            scopes_base: self.scopes.len() - 1,
            captured_names,
        });
        for param in params {
            let slot = self.declare(param, false)?;
            if self.current_captured(param) {
                self.emit(Instruction::make_cell(slot));
            }
        }
        match expr_body {
            Some(expression) => {
                self.expression(expression)?;
                self.emit(Instruction::return_());
            }
            None => {
                let last_keeps_value =
                    wants_last_value && matches!(body.last(), Some(Stmt::Expression(_)));
                for (i, statement) in body.iter().enumerate() {
                    let wants_value = last_keeps_value && i + 1 == body.len();
                    self.statement(statement, wants_value)?;
                }
                // The implicit-return tail is skipped when the body ends
                // with an explicit `return` (which already emitted its own).
                if !matches!(body.last(), Some(Stmt::Return(_))) {
                    if !last_keeps_value {
                        self.emit(Instruction::load_undefined());
                    }
                    self.emit(Instruction::return_());
                }
            }
        }
        let state = self.states.pop().expect("compiling a function");
        self.scopes.pop();
        self.functions[index].local_count = state.local_count as u32;
        self.completed.push(state);
        let index = self.completed.len() - 1;
        let captures = self.completed[index].captures.clone();
        Ok((self.completed[index].index, captures))
    }

    fn current_captured(&self, name: &str) -> bool {
        self.states
            .last()
            .expect("compiling a function")
            .captured_names
            .contains(name)
    }

    /// Computes the set of names bound in this function that any nested
    /// function references (shadowing-aware). Those bindings become cells.
    fn collect_captures(
        params: &[String],
        body: &[Stmt],
        expr_body: Option<&Expr>,
    ) -> HashSet<String> {
        let mut bound: HashSet<String> = params.iter().cloned().collect();
        let mut captured = HashSet::new();
        let mut free = HashSet::new();
        Self::walk_statements(body, &mut bound, &mut captured, &mut free);
        if let Some(expression) = expr_body {
            Self::walk_expr(expression, &mut bound, &mut captured, &mut free);
        }
        captured
    }

    fn walk_statements(
        statements: &[Stmt],
        bound: &mut HashSet<String>,
        captured: &mut HashSet<String>,
        free: &mut HashSet<String>,
    ) {
        for statement in statements {
            Self::walk_statement(statement, bound, captured, free);
        }
    }

    fn walk_statement(
        statement: &Stmt,
        bound: &mut HashSet<String>,
        captured: &mut HashSet<String>,
        free: &mut HashSet<String>,
    ) {
        match statement {
            Stmt::Expression(expression) => Self::walk_expr(expression, bound, captured, free),
            Stmt::Empty => {}
            Stmt::Let(declarators) | Stmt::Const(declarators) => {
                for declarator in declarators {
                    bound.insert(declarator.name.clone());
                    if let Some(init) = &declarator.init {
                        Self::walk_expr(init, bound, captured, free);
                    }
                }
            }
            Stmt::Block(statements) => {
                Self::walk_statements(statements, bound, captured, free);
            }
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => {
                Self::walk_expr(condition, bound, captured, free);
                Self::walk_statement(then_body, bound, captured, free);
                if let Some(else_body) = else_body {
                    Self::walk_statement(else_body, bound, captured, free);
                }
            }
            Stmt::While { condition, body } => {
                Self::walk_expr(condition, bound, captured, free);
                Self::walk_statement(body, bound, captured, free);
            }
            Stmt::For {
                init,
                condition,
                update,
                body,
            } => {
                if let Some(init) = init {
                    Self::walk_statement(init, bound, captured, free);
                }
                if let Some(condition) = condition {
                    Self::walk_expr(condition, bound, captured, free);
                }
                if let Some(update) = update {
                    Self::walk_expr(update, bound, captured, free);
                }
                Self::walk_statement(body, bound, captured, free);
            }
            Stmt::Function { name, params, body } => {
                bound.insert(name.clone());
                Self::walk_nested_function(params, body, None, bound, captured, free);
            }
            Stmt::Return(Some(expression)) => Self::walk_expr(expression, bound, captured, free),
            Stmt::Return(None) => {}
        }
    }

    fn walk_expr(
        expression: &Expr,
        bound: &mut HashSet<String>,
        captured: &mut HashSet<String>,
        free: &mut HashSet<String>,
    ) {
        match expression {
            Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Null | Expr::Undefined => {}
            Expr::String(_) => {}
            Expr::Var(name) => {
                if !bound.contains(name) {
                    free.insert(name.clone());
                }
            }
            Expr::Template(parts) => {
                for part in parts {
                    if let TemplatePart::Substitution(substitution) = part {
                        Self::walk_expr(substitution, bound, captured, free);
                    }
                }
            }
            Expr::FunctionExpr { params, body } => {
                Self::walk_nested_function(params, body, None, bound, captured, free)
            }
            Expr::Arrow { params, body } => match body {
                ArrowBody::Block(statements) => {
                    Self::walk_nested_function(params, statements, None, bound, captured, free)
                }
                ArrowBody::Expression(expression) => {
                    Self::walk_nested_function(params, &[], Some(expression), bound, captured, free)
                }
            },
            Expr::Assign(_, target, value) => {
                // Walk the target's sub-expressions (receivers/keys can be
                // arbitrary expressions that nested functions may capture)
                // without treating the target itself as a free reference.
                match target.as_ref() {
                    Expr::Var(name) => {
                        if !bound.contains(name) {
                            free.insert(name.clone());
                        }
                    }
                    Expr::Property(receiver, _) => Self::walk_expr(receiver, bound, captured, free),
                    Expr::Index(receiver, key) => {
                        Self::walk_expr(receiver, bound, captured, free);
                        Self::walk_expr(key, bound, captured, free);
                    }
                    _ => {}
                }
                Self::walk_expr(value, bound, captured, free);
            }
            Expr::Binary(_, left, right) => {
                Self::walk_expr(left, bound, captured, free);
                Self::walk_expr(right, bound, captured, free);
            }
            Expr::Unary(_, operand) => Self::walk_expr(operand, bound, captured, free),
            Expr::Call(callee, arguments) => {
                Self::walk_expr(callee, bound, captured, free);
                for argument in arguments {
                    Self::walk_expr(argument, bound, captured, free);
                }
            }
            Expr::Array(elements) => {
                for element in elements.iter().flatten() {
                    Self::walk_expr(element, bound, captured, free);
                }
            }
            Expr::Object(properties) => {
                for property in properties {
                    if let ObjectKey::Computed(key) = &property.key {
                        Self::walk_expr(key, bound, captured, free);
                    }
                    match &property.value {
                        Some(value) => Self::walk_expr(value, bound, captured, free),
                        // Shorthand references the key name.
                        None => {
                            if let ObjectKey::Ident(name) = &property.key
                                && !bound.contains(name)
                            {
                                free.insert(name.clone());
                            }
                        }
                    }
                }
            }
            Expr::Property(receiver, _) => Self::walk_expr(receiver, bound, captured, free),
            Expr::Index(receiver, key) => {
                Self::walk_expr(receiver, bound, captured, free);
                Self::walk_expr(key, bound, captured, free);
            }
            Expr::ConsoleLog(argument) => Self::walk_expr(argument, bound, captured, free),
        }
    }

    /// Walks a nested function body with a fresh binding set: names free in
    /// the child that are bound here become captures; otherwise they
    /// propagate upward as free.
    fn walk_nested_function(
        params: &[String],
        body: &[Stmt],
        expr_body: Option<&Expr>,
        bound: &mut HashSet<String>,
        captured: &mut HashSet<String>,
        free: &mut HashSet<String>,
    ) {
        let mut child_bound: HashSet<String> = params.iter().cloned().collect();
        let mut child_captured = HashSet::new();
        let mut child_free = HashSet::new();
        Self::walk_statements(body, &mut child_bound, &mut child_captured, &mut child_free);
        if let Some(expression) = expr_body {
            Self::walk_expr(
                expression,
                &mut child_bound,
                &mut child_captured,
                &mut child_free,
            );
        }
        for name in child_free {
            if bound.contains(&name) {
                captured.insert(name);
            } else {
                free.insert(name);
            }
        }
    }

    fn expression(&mut self, expression: &Expr) -> Result<(), CompileError> {
        match expression {
            Expr::Int(value) => {
                if *value >= i32::MIN as i64 && *value <= i32::MAX as i64 {
                    self.emit(Instruction::load_int(*value));
                } else {
                    // Integers beyond `i32` go through the float path: exact
                    // up to 2^53, matching JS double semantics.
                    for instruction in Instruction::load_f64(*value as f64) {
                        self.emit(instruction);
                    }
                }
            }
            Expr::Float(value) => {
                for instruction in Instruction::load_f64(*value) {
                    self.emit(instruction);
                }
            }
            Expr::Bool(value) => {
                self.emit(Instruction::load_bool(*value));
            }
            Expr::Null => {
                self.emit(Instruction::load_null());
            }
            Expr::Undefined => {
                self.emit(Instruction::load_undefined());
            }
            Expr::Var(name) => match self.resolve(name) {
                Some(Resolved::Local { slot, .. }) => {
                    self.emit(Instruction::get_local(slot));
                }
                Some(Resolved::Upvalue { index, .. }) => {
                    self.emit(Instruction::get_upvalue(index));
                }
                None => return Err(CompileError::Undeclared(name.clone())),
            },
            Expr::String(text) => {
                let index = self.string_index(text);
                self.emit(Instruction::load_string(index));
            }
            Expr::Array(elements) => {
                for element in elements {
                    match element {
                        Some(expression) => self.expression(expression)?,
                        None => {
                            self.emit(Instruction::load_undefined());
                        }
                    }
                }
                self.emit(Instruction::new_array(elements.len()));
            }
            Expr::Object(properties) => {
                for property in properties {
                    // Keys push first: the machine pops key/value pairs.
                    match &property.key {
                        ObjectKey::Ident(name) | ObjectKey::Literal(name) => {
                            let index = self.string_index(name);
                            self.emit(Instruction::load_string(index));
                        }
                        ObjectKey::Computed(expression) => {
                            self.expression(expression)?;
                            self.emit(Instruction::unary(r8_abi::OP_TO_STRING));
                        }
                    }
                    match &property.value {
                        Some(value) => self.expression(value)?,
                        None => {
                            // Shorthand: the value is a reference to the key.
                            let name = match &property.key {
                                ObjectKey::Ident(name) => name,
                                _ => unreachable!("shorthand requires an identifier key"),
                            };
                            match self.resolve(name) {
                                Some(Resolved::Local { slot, .. }) => {
                                    self.emit(Instruction::get_local(slot));
                                }
                                Some(Resolved::Upvalue { index, .. }) => {
                                    self.emit(Instruction::get_upvalue(index));
                                }
                                None => return Err(CompileError::Undeclared(name.clone())),
                            }
                        }
                    }
                }
                self.emit(Instruction::new_object(properties.len()));
            }
            Expr::Property(receiver, name) => {
                // `receiver.name` is `receiver["name"]` (one GetV opcode).
                self.expression(receiver)?;
                let index = self.string_index(name);
                self.emit(Instruction::load_string(index));
                self.emit(Instruction::get_index());
            }
            Expr::Index(receiver, key) => {
                self.expression(receiver)?;
                self.expression(key)?;
                self.emit(Instruction::get_index());
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
            Expr::FunctionExpr { params, body } => {
                let (index, captures) = self.enter_function(params, body, None, false)?;
                self.emit_closure_creation(index, &captures);
            }
            Expr::Arrow { params, body } => {
                let (index, captures) = match body {
                    ArrowBody::Block(statements) => {
                        self.enter_function(params, statements, None, false)?
                    }
                    ArrowBody::Expression(expression) => {
                        self.enter_function(params, &[], Some(expression), false)?
                    }
                };
                self.emit_closure_creation(index, &captures);
            }
            Expr::Call(callee, arguments) => {
                // Callee first, then arguments: the machine pops
                // `[fn, arg1..argN]`.
                self.expression(callee)?;
                for argument in arguments {
                    self.expression(argument)?;
                }
                self.emit(Instruction::call(arguments.len()));
            }
            Expr::Assign(op, target, value) => match target.as_ref() {
                Expr::Var(name) => {
                    let resolved = match self.resolve(name) {
                        Some(resolved) => resolved,
                        None => return Err(CompileError::Undeclared(name.clone())),
                    };
                    let (slot, index, is_const) = match resolved {
                        Resolved::Local { slot, is_const } => (Some(slot), None, is_const),
                        Resolved::Upvalue { index, is_const } => (None, Some(index), is_const),
                    };
                    if is_const {
                        return Err(CompileError::ConstAssignment(name.clone()));
                    }
                    if *op != AssignOp::Assign {
                        // Compound assignment reads the target first.
                        if let Some(slot) = slot {
                            self.emit(Instruction::get_local(slot));
                        }
                        if let Some(index) = index {
                            self.emit(Instruction::get_upvalue(index));
                        }
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
                    if let Some(slot) = slot {
                        self.emit(Instruction::set_local(slot));
                    }
                    if let Some(index) = index {
                        self.emit(Instruction::set_upvalue(index));
                    }
                }
                Expr::Property(receiver, name) => {
                    // Evaluate the receiver once, then:
                    //   plain:    recv; value; key; set; pop -> value
                    //   compound: recv; dup; key; get; value; op; key;
                    //             set; pop -> new value
                    // (`set_index` pops the key off the top, then the value,
                    // and keeps the receiver, pushing the value back — so
                    // both shapes end `[recv, value]` before the pop.)
                    self.expression(receiver)?;
                    let key_index = self.string_index(name);
                    if *op != AssignOp::Assign {
                        self.emit(Instruction::dup());
                        self.emit(Instruction::load_string(key_index));
                        self.emit(Instruction::get_index());
                        self.expression(value)?;
                        let opcode = match op {
                            AssignOp::AddAssign => BinaryOp::Add,
                            AssignOp::SubAssign => BinaryOp::Sub,
                            AssignOp::MulAssign => BinaryOp::Mul,
                            AssignOp::DivAssign => BinaryOp::Div,
                            AssignOp::ModAssign => BinaryOp::Mod,
                            AssignOp::Assign => unreachable!(),
                        };
                        self.emit(Instruction::binary(
                            opcode.opcode().expect("non-logical op"),
                        ));
                        self.emit(Instruction::load_string(key_index));
                    } else {
                        self.expression(value)?;
                        self.emit(Instruction::load_string(key_index));
                    }
                    self.emit(Instruction::set_index());
                    self.emit(Instruction::pop());
                }
                Expr::Index(receiver, key) => {
                    // Same shape as property assignment, with a computed key
                    // (evaluated once per read/write — twice for compound).
                    self.expression(receiver)?;
                    if *op != AssignOp::Assign {
                        self.emit(Instruction::dup());
                        self.expression(key)?;
                        self.emit(Instruction::get_index());
                        self.expression(value)?;
                        let opcode = match op {
                            AssignOp::AddAssign => BinaryOp::Add,
                            AssignOp::SubAssign => BinaryOp::Sub,
                            AssignOp::MulAssign => BinaryOp::Mul,
                            AssignOp::DivAssign => BinaryOp::Div,
                            AssignOp::ModAssign => BinaryOp::Mod,
                            AssignOp::Assign => unreachable!(),
                        };
                        self.emit(Instruction::binary(
                            opcode.opcode().expect("non-logical op"),
                        ));
                        self.expression(key)?;
                    } else {
                        self.expression(value)?;
                        self.expression(key)?;
                    }
                    self.emit(Instruction::set_index());
                    self.emit(Instruction::pop());
                }
                // The parser only accepts variables, properties, and
                // computed keys as assignment targets.
                _ => return Err(CompileError::Undeclared(String::new())),
            },
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
                    let end = self
                        .states
                        .last()
                        .expect("compiling a function")
                        .buffer
                        .len();
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

    fn emit_closure_creation(&mut self, index: usize, captures: &[Capture]) {
        self.emit(Instruction::closure(index));
        for capture in captures {
            match capture {
                Capture::Local(slot) => {
                    self.emit(Instruction::capture_local(*slot));
                }
                Capture::Upvalue(upvalue) => {
                    self.emit(Instruction::capture_upvalue(*upvalue));
                }
            }
        }
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
                    if self.current_captured(&declarator.name) {
                        self.emit(Instruction::make_cell(slot));
                    }
                }
            }
            Stmt::Block(statements) => {
                self.scopes.push(Scope {
                    bindings: HashMap::new(),
                });
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
                        let else_start = self
                            .states
                            .last()
                            .expect("compiling a function")
                            .buffer
                            .len();
                        self.patch(else_jump, else_start);
                        self.statement(else_body, false)?;
                        let end = self
                            .states
                            .last()
                            .expect("compiling a function")
                            .buffer
                            .len();
                        self.patch(end_jump, end);
                    }
                    None => {
                        let end = self
                            .states
                            .last()
                            .expect("compiling a function")
                            .buffer
                            .len();
                        self.patch(else_jump, end);
                    }
                }
            }
            Stmt::While { condition, body } => {
                let loop_start = self
                    .states
                    .last()
                    .expect("compiling a function")
                    .buffer
                    .len();
                self.expression(condition)?;
                let end_jump = self.emit(Instruction::jmp_if_false(0));
                self.statement(body, false)?;
                self.emit(Instruction::jmp(loop_start));
                let end = self
                    .states
                    .last()
                    .expect("compiling a function")
                    .buffer
                    .len();
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
                self.scopes.push(Scope {
                    bindings: HashMap::new(),
                });
                if let Some(init) = init {
                    self.statement(init, false)?;
                }
                let loop_start = self
                    .states
                    .last()
                    .expect("compiling a function")
                    .buffer
                    .len();
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
                let end = self
                    .states
                    .last()
                    .expect("compiling a function")
                    .buffer
                    .len();
                self.patch(end_jump, end);
                self.scopes.pop();
            }
            Stmt::Function { name, params, body } => {
                let slot = self.declare(name, false)?;
                // The function's own cell exists before its closure is
                // created, so a self-referencing body (recursion) captures
                // it and reads the binding once it is assigned.
                let (index, captures) = self.enter_function(params, body, None, false)?;
                self.emit(Instruction::load_undefined());
                self.emit(Instruction::set_local(slot));
                if self.current_captured(name) {
                    self.emit(Instruction::make_cell(slot));
                }
                self.emit_closure_creation(index, &captures);
                self.emit(Instruction::set_local(slot));
            }
            Stmt::Return(Some(expression)) => {
                self.expression(expression)?;
                self.emit(Instruction::return_());
            }
            Stmt::Return(None) => {
                self.emit(Instruction::load_undefined());
                self.emit(Instruction::return_());
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
            Expr::Assign(
                AssignOp::Assign,
                Box::new(Expr::Var("x".into())),
                Box::new(Expr::Int(5))
            )
        );
        // Assignment is right-associative and lowest precedence.
        assert_eq!(
            parse("x = y = 5").unwrap(),
            Expr::Assign(
                AssignOp::Assign,
                Box::new(Expr::Var("x".into())),
                Box::new(Expr::Assign(
                    AssignOp::Assign,
                    Box::new(Expr::Var("y".into())),
                    Box::new(Expr::Int(5))
                ))
            )
        );
        assert_eq!(
            parse("x += 1").unwrap(),
            Expr::Assign(
                AssignOp::AddAssign,
                Box::new(Expr::Var("x".into())),
                Box::new(Expr::Int(1))
            )
        );
        // Addition binds tighter than assignment.
        assert_eq!(
            parse("x = 1 + 2").unwrap(),
            Expr::Assign(
                AssignOp::Assign,
                Box::new(Expr::Var("x".into())),
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

    #[test]
    fn lexes_function_and_arrow_tokens() {
        assert_eq!(
            tokenize("function =>").unwrap(),
            vec![Token::Function, Token::EqArrow, Token::Eof]
        );
        // `=>` is distinct from `==` and `=`.
        assert_eq!(
            tokenize("a == b = c").unwrap(),
            vec![
                Token::Identifier("a".into()),
                Token::EqEq,
                Token::Identifier("b".into()),
                Token::EqAssign,
                Token::Identifier("c".into()),
                Token::Eof
            ]
        );
    }

    #[test]
    fn parses_function_declaration() {
        let program = parse_program("function add(a, b) { return a + b; }").unwrap();
        assert_eq!(program.len(), 1);
        assert_eq!(
            program[0],
            Stmt::Function {
                name: "add".into(),
                params: vec!["a".into(), "b".into()],
                body: vec![Stmt::Return(Some(Expr::Binary(
                    BinaryOp::Add,
                    Box::new(Expr::Var("a".into())),
                    Box::new(Expr::Var("b".into()))
                )))]
            }
        );
    }

    #[test]
    fn parses_return_statements() {
        let program = parse_program("function f() { return; }").unwrap();
        assert_eq!(
            program[0],
            Stmt::Function {
                name: "f".into(),
                params: vec![],
                body: vec![Stmt::Return(None)]
            }
        );
    }

    #[test]
    fn parses_arrow_forms() {
        // Single param without parens.
        assert_eq!(
            parse("x => x * 2").unwrap(),
            Expr::Arrow {
                params: vec!["x".into()],
                body: ArrowBody::Expression(Box::new(Expr::Binary(
                    BinaryOp::Mul,
                    Box::new(Expr::Var("x".into())),
                    Box::new(Expr::Int(2))
                )))
            }
        );
        // Multiple params with parens and a block body.
        let arrow = parse("(a, b) => { return a + b; }").unwrap();
        assert_eq!(
            arrow,
            Expr::Arrow {
                params: vec!["a".into(), "b".into()],
                body: ArrowBody::Block(vec![Stmt::Return(Some(Expr::Binary(
                    BinaryOp::Add,
                    Box::new(Expr::Var("a".into())),
                    Box::new(Expr::Var("b".into()))
                )))])
            }
        );
        // Zero params.
        assert_eq!(
            parse("() => 42").unwrap(),
            Expr::Arrow {
                params: vec![],
                body: ArrowBody::Expression(Box::new(Expr::Int(42)))
            }
        );
    }

    #[test]
    fn parens_are_not_always_arrows() {
        // A parenthesized expression is not arrow params.
        assert_eq!(
            parse("(a + b)").unwrap(),
            Expr::Binary(
                BinaryOp::Add,
                Box::new(Expr::Var("a".into())),
                Box::new(Expr::Var("b".into()))
            )
        );
        // A lone parenthesized identifier followed by more tokens.
        assert!(parse("(a) + 1").is_ok());
    }

    #[test]
    fn parses_calls() {
        assert_eq!(
            parse("add(2, 3)").unwrap(),
            Expr::Call(
                Box::new(Expr::Var("add".into())),
                vec![Expr::Int(2), Expr::Int(3)]
            )
        );
        // Calls chain, and arrows can be arguments.
        assert!(matches!(parse("f(x)(y)").unwrap(), Expr::Call(_, _)));
        assert!(matches!(
            parse("twice((v) => v + 1, 5)").unwrap(),
            Expr::Call(_, _)
        ));
    }

    #[test]
    fn parses_function_expression() {
        assert!(matches!(
            parse("function (x) { return x; }").unwrap(),
            Expr::FunctionExpr { .. }
        ));
        assert_eq!(
            parse("function named() {}").unwrap_err(),
            ParseError::Unsupported("named function expressions")
        );
    }

    #[test]
    fn rejects_unsupported_parameters() {
        assert_eq!(
            parse_program("function f(a, b = 1) {}").unwrap_err(),
            ParseError::Unsupported("default parameters")
        );
        assert_eq!(
            parse_program("function f(1) {}").unwrap_err(),
            ParseError::Unsupported("only simple parameter names")
        );
    }

    #[test]
    fn compiles_function_declaration_with_table_entry() {
        let program = parse_program("function f() { return 7; } f()").unwrap();
        let compiled = compile_program(&program).unwrap();
        // Buffers concatenate in completion order, so f's body (which
        // completed first) precedes main; execution starts at
        // functions[0].start.
        assert_eq!(
            compiled.instructions,
            vec![
                // f's body
                Instruction::load_int(7),
                Instruction::return_(),
                // main: declaration creation sequence, then the call site
                Instruction::load_undefined(),
                Instruction::set_local(0),
                Instruction::closure(1),
                Instruction::set_local(0),
                // f()
                Instruction::get_local(0),
                Instruction::call(0),
                Instruction::return_(),
            ]
        );
        assert_eq!(compiled.functions.len(), 2);
        assert_eq!(
            compiled.functions[0],
            KeelFunction {
                start: 2,
                len: 7,
                local_count: 1,
                arity: 0,
            }
        );
        assert_eq!(
            compiled.functions[1],
            KeelFunction {
                start: 0,
                len: 2,
                local_count: 0,
                arity: 0,
            }
        );
    }

    #[test]
    fn compiles_captured_locals_with_cells() {
        let program = parse_program("let c = 0; const inc = () => { c += 1; }; inc(); c").unwrap();
        let compiled = compile_program(&program).unwrap();
        let instructions = &compiled.instructions;
        // Main starts after the arrow's body in the flat stream.
        let main_start = compiled.functions[0].start as usize;
        // c's declaration ends with MAKE_CELL; the closure captures it.
        assert_eq!(
            instructions[main_start..main_start + 6],
            vec![
                Instruction::load_int(0),
                Instruction::set_local(0),
                Instruction::make_cell(0),
                Instruction::closure(1),
                Instruction::capture_local(0),
                Instruction::set_local(1),
            ]
        );
        // The arrow body reads and writes the upvalue; the statement value
        // is popped and the implicit-return tail follows.
        let body_start = compiled.functions[1].start as usize;
        assert_eq!(
            instructions[body_start..body_start + 8],
            vec![
                Instruction::get_upvalue(0),
                Instruction::load_int(1),
                Instruction::binary(OP_ADD),
                Instruction::dup(),
                Instruction::set_upvalue(0),
                Instruction::pop(),
                Instruction::load_undefined(),
                Instruction::return_(),
            ]
        );
    }

    #[test]
    fn compiles_transitive_captures() {
        let program = parse_program("let x = 1; const outer = () => () => x;").unwrap();
        let compiled = compile_program(&program).unwrap();
        let instructions = &compiled.instructions;
        // The inner arrow's body comes first (completion order).
        let inner_start = compiled.functions[2].start as usize;
        assert_eq!(
            instructions[inner_start..inner_start + 2],
            vec![Instruction::get_upvalue(0), Instruction::return_()]
        );
        // The outer arrow's body is itself an expression body arrow: it
        // creates the inner closure and returns it.
        let outer_start = compiled.functions[1].start as usize;
        assert_eq!(
            instructions[outer_start..outer_start + 3],
            vec![
                Instruction::closure(2),
                Instruction::capture_upvalue(0),
                Instruction::return_(),
            ]
        );
        // Main: x's declaration (captured, so it gets a cell), then
        // closure(1); capture_local(0); set_local(1) for `outer`.
        let main_start = compiled.functions[0].start as usize;
        assert_eq!(
            instructions[main_start + 3..main_start + 6],
            vec![
                Instruction::closure(1),
                Instruction::capture_local(0),
                Instruction::set_local(1),
            ]
        );
    }

    #[test]
    fn compiles_recursion_through_self_capture() {
        let program = parse_program(
            "function fact(n) { if (n <= 1) { return 1; } return n * fact(n - 1); } fact(5)",
        )
        .unwrap();
        let compiled = compile_program(&program).unwrap();
        let instructions = &compiled.instructions;
        // fact's body comes first (completion order); it reads fact through
        // its upvalue cell for the recursive call.
        let body_start = compiled.functions[1].start as usize;
        assert!(
            instructions[body_start..]
                .iter()
                .any(|i| i.op == r8_abi::OP_GET_UPVALUE)
        );
        // Jumps inside the body are rebased to the flat stream. The `if`
        // jumps past its two-instruction then-branch (load + return).
        let jump = instructions[body_start..]
            .iter()
            .find(|i| i.op == r8_abi::OP_JMP_IF_FALSE)
            .unwrap();
        assert_eq!(jump.operand as usize, body_start + 6);
        // Main: fact's declaration pre-creates its cell and self-captures.
        let main_start = compiled.functions[0].start as usize;
        assert_eq!(
            instructions[main_start..main_start + 5],
            vec![
                Instruction::load_undefined(),
                Instruction::set_local(0),
                Instruction::make_cell(0),
                Instruction::closure(1),
                Instruction::capture_local(0),
            ]
        );
    }
}

#[cfg(test)]
mod phase5_tests {
    use super::*;

    #[test]
    fn lexes_brackets_and_colons() {
        assert_eq!(
            tokenize("[ ] { } :").unwrap(),
            vec![
                Token::LeftBracket,
                Token::RightBracket,
                Token::LeftBrace,
                Token::RightBrace,
                Token::Colon,
                Token::Eof
            ]
        );
    }

    #[test]
    fn parses_array_literals() {
        assert_eq!(
            parse("[1, 2, 3]").unwrap(),
            Expr::Array(vec![
                Some(Expr::Int(1)),
                Some(Expr::Int(2)),
                Some(Expr::Int(3))
            ])
        );
        // Holes and a trailing comma.
        assert_eq!(
            parse("[1,,3,]").unwrap(),
            Expr::Array(vec![Some(Expr::Int(1)), None, Some(Expr::Int(3))])
        );
        assert!(parse("[]").is_ok());
    }

    #[test]
    fn parses_object_literals() {
        assert_eq!(
            parse("({ a: 1 })").unwrap(),
            Expr::Object(vec![ObjectProperty {
                key: ObjectKey::Ident("a".into()),
                value: Some(Expr::Int(1)),
            }])
        );
        // Shorthand, string keys, computed keys.
        assert_eq!(
            parse("({ b, 'c d': 2, [k]: 3 })").unwrap(),
            Expr::Object(vec![
                ObjectProperty {
                    key: ObjectKey::Ident("b".into()),
                    value: None
                },
                ObjectProperty {
                    key: ObjectKey::Literal("c d".into()),
                    value: Some(Expr::Int(2))
                },
                ObjectProperty {
                    key: ObjectKey::Computed(Box::new(Expr::Var("k".into()))),
                    value: Some(Expr::Int(3))
                },
            ])
        );
    }

    #[test]
    fn parses_property_and_index_access() {
        assert_eq!(
            parse("a.b.c").unwrap(),
            Expr::Property(
                Box::new(Expr::Property(Box::new(Expr::Var("a".into())), "b".into())),
                "c".into()
            )
        );
        assert_eq!(
            parse("a[0]").unwrap(),
            Expr::Index(Box::new(Expr::Var("a".into())), Box::new(Expr::Int(0)))
        );
        // Reserved words are legal property names.
        assert_eq!(
            parse("a.for").unwrap(),
            Expr::Property(Box::new(Expr::Var("a".into())), "for".into())
        );
    }

    #[test]
    fn parses_method_calls_and_property_assignment() {
        // .map chains naturally through property + call postfix.
        assert!(matches!(
            parse("values.map((value) => value * 2)").unwrap(),
            Expr::Call(_, _)
        ));
        let assigned = parse("a.b = 5").unwrap();
        assert!(matches!(
            assigned,
            Expr::Assign(_, ref target, _) if matches!(target.as_ref(), Expr::Property(_, _))
        ));
        let compound = parse("a[0] += 1").unwrap();
        assert!(matches!(
            compound,
            Expr::Assign(_, ref target, _) if matches!(target.as_ref(), Expr::Index(_, _))
        ));
        // Non-targets still reject.
        assert_eq!(
            parse("1 = 2").unwrap_err(),
            ParseError::Unsupported("only simple assignment targets")
        );
    }

    #[test]
    fn compiles_array_literal_with_elements() {
        let compiled = compile(&parse("[1, 2]").unwrap()).unwrap();
        assert_eq!(
            compiled.instructions,
            vec![
                Instruction::load_int(1),
                Instruction::load_int(2),
                Instruction::new_array(2),
                Instruction::return_()
            ]
        );
    }

    #[test]
    fn compiles_object_literal_key_value_pairs() {
        let compiled = compile(&parse("({ a: 1 })").unwrap()).unwrap();
        assert_eq!(
            compiled.instructions,
            vec![
                Instruction::load_string(0),
                Instruction::load_int(1),
                Instruction::new_object(1),
                Instruction::return_()
            ]
        );
        assert_eq!(compiled.pool[0].as_str(), Some("a"));
    }

    #[test]
    fn compiles_property_access_as_string_index() {
        let program = parse_program("let a = [1]; a.length").unwrap();
        let compiled = compile_program(&program).unwrap();
        let instructions = &compiled.instructions;
        let main_start = compiled.functions[0].start as usize;
        // `let a = [1]` is 3 instructions; then a.length -> get_local;
        // load "length"; get_index
        assert_eq!(
            instructions[main_start + 3..main_start + 6],
            vec![
                Instruction::get_local(0),
                Instruction::load_string(0), // pool: 0 = "length"
                Instruction::get_index(),
            ]
        );
        // The pool holds only "length" here ("1" is an int literal).
        assert_eq!(compiled.pool.len(), 1);
        assert_eq!(compiled.pool[0].as_str(), Some("length"));
    }

    #[test]
    fn compiles_property_assignment_with_receiver_once() {
        let program = parse_program("let o = {}; o.a = 5;").unwrap();
        let compiled = compile_program(&program).unwrap();
        let instructions = &compiled.instructions;
        let main_start = compiled.functions[0].start as usize;
        // `let o = {}` is 2 instructions; then o.a = 5:
        // get o; load 5; load "a"; set_index; pop
        assert_eq!(
            instructions[main_start + 2..main_start + 7],
            vec![
                Instruction::get_local(0),
                Instruction::load_int(5),
                Instruction::load_string(0),
                Instruction::set_index(),
                Instruction::pop(),
            ]
        );
    }

    #[test]
    fn compiles_compound_property_assignment() {
        let program = parse_program("let o = {}; o.n += 2;").unwrap();
        let compiled = compile_program(&program).unwrap();
        let instructions = &compiled.instructions;
        let main_start = compiled.functions[0].start as usize;
        // `let o = {}` is 2 instructions; then o.n += 2:
        // get o; dup; key; get; load 2; add; key; set; pop
        assert_eq!(
            instructions[main_start + 2..main_start + 11],
            vec![
                Instruction::get_local(0),
                Instruction::dup(),
                Instruction::load_string(0),
                Instruction::get_index(),
                Instruction::load_int(2),
                Instruction::binary(r8_abi::OP_ADD),
                Instruction::load_string(0),
                Instruction::set_index(),
                Instruction::pop(),
            ]
        );
    }
}
