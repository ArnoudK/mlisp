use crate::error::CompileError;
use crate::frontend::ast::{Expr, ExprKind, Program};
use crate::frontend::lexer::{Token, TokenKind, lex};
use crate::span::Span;

pub struct Parser {
    tokens: Vec<Token>,
    cursor: usize,
}

impl Parser {
    pub fn new(source: &str) -> Result<Self, CompileError> {
        let tokens = lex(source)?;
        Ok(Self { tokens, cursor: 0 })
    }

    pub fn parse_program(self) -> Result<Program, CompileError> {
        let mut parser = self;
        parser.tokens = lex_from_tokens(parser.tokens)?;
        let mut forms = Vec::new();

        while !parser.is_eof() {
            forms.push(parser.parse_expr()?);
        }

        Ok(Program { forms })
    }

    fn parse_expr(&mut self) -> Result<Expr, CompileError> {
        let token = self
            .advance()
            .ok_or_else(|| CompileError::Parse("unexpected end of file".into()))?;

        match token.kind {
            TokenKind::Integer(value) => Ok(Expr {
                kind: ExprKind::Integer(value),
                span: token.span,
            }),
            TokenKind::Boolean(value) => Ok(Expr {
                kind: ExprKind::Boolean(value),
                span: token.span,
            }),
            TokenKind::Char(value) => Ok(Expr {
                kind: ExprKind::Char(value),
                span: token.span,
            }),
            TokenKind::String(value) => Ok(Expr {
                kind: ExprKind::String(value),
                span: token.span,
            }),
            TokenKind::Symbol(symbol) => Ok(Expr {
                kind: ExprKind::Symbol(symbol),
                span: token.span,
            }),
            TokenKind::Quote => {
                let expr = self.parse_expr()?;
                Ok(Expr {
                    kind: ExprKind::Quote(Box::new(expr.clone())),
                    span: token.span.merge(expr.span),
                })
            }
            TokenKind::LParen => self.parse_list(token.span.start),
            TokenKind::RParen => Err(CompileError::Parse(format!(
                "unexpected ')' at byte {}",
                token.span.start
            ))),
        }
    }

    fn parse_list(&mut self, start: usize) -> Result<Expr, CompileError> {
        let mut items = Vec::new();
        loop {
            let Some(token) = self.peek() else {
                return Err(CompileError::Parse(format!(
                    "unterminated list starting at byte {start}"
                )));
            };

            if matches!(token.kind, TokenKind::RParen) {
                let end = token.span.end;
                self.cursor += 1;
                return Ok(Expr {
                    kind: ExprKind::List(items),
                    span: Span { start, end },
                });
            }

            items.push(self.parse_expr()?);
        }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.cursor)
    }

    fn advance(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.cursor).cloned();
        self.cursor += usize::from(token.is_some());
        token
    }

    fn is_eof(&self) -> bool {
        self.cursor >= self.tokens.len()
    }
}

fn lex_from_tokens(tokens: Vec<Token>) -> Result<Vec<Token>, CompileError> {
    if tokens.is_empty() {
        return Ok(tokens);
    }

    let mut normalized = Vec::with_capacity(tokens.len());
    for token in tokens {
        match &token.kind {
            TokenKind::Symbol(symbol) if symbol.is_empty() => {
                return Err(CompileError::Lex("empty symbol is not allowed".into()));
            }
            _ => normalized.push(token),
        }
    }
    Ok(normalized)
}
