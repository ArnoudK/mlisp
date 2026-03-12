use crate::error::CompileError;
use crate::span::Span;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenKind {
    LParen,
    RParen,
    Dot,
    Quote,
    Integer(i64),
    Boolean(bool),
    Char(char),
    String(String),
    Symbol(String),
}

pub fn lex(source: &str) -> Result<Vec<Token>, CompileError> {
    let mut tokens = Vec::new();
    let bytes = source.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        let byte = bytes[index];
        match byte {
            b' ' | b'\n' | b'\r' | b'\t' => {
                index += 1;
            }
            b';' => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'(' => {
                tokens.push(Token {
                    kind: TokenKind::LParen,
                    span: Span {
                        start: index,
                        end: index + 1,
                    },
                });
                index += 1;
            }
            b')' => {
                tokens.push(Token {
                    kind: TokenKind::RParen,
                    span: Span {
                        start: index,
                        end: index + 1,
                    },
                });
                index += 1;
            }
            b'.' if index + 1 == bytes.len() || is_delimiter(bytes[index + 1]) => {
                tokens.push(Token {
                    kind: TokenKind::Dot,
                    span: Span {
                        start: index,
                        end: index + 1,
                    },
                });
                index += 1;
            }
            b'\'' => {
                tokens.push(Token {
                    kind: TokenKind::Quote,
                    span: Span {
                        start: index,
                        end: index + 1,
                    },
                });
                index += 1;
            }
            b'#' => {
                if index + 1 >= bytes.len() {
                    return Err(CompileError::Lex("unterminated boolean literal".into()));
                }

                let span = Span {
                    start: index,
                    end: index + 2,
                };
                let kind = match bytes[index + 1] {
                    b't' => TokenKind::Boolean(true),
                    b'f' => TokenKind::Boolean(false),
                    b'\\' => {
                        let start = index;
                        let mut end = index + 2;
                        while end < bytes.len() && !is_delimiter(bytes[end]) {
                            end += 1;
                        }
                        let literal = &source[index + 2..end];
                        let ch = match literal {
                            "space" => ' ',
                            "newline" => '\n',
                            _ => {
                                let mut chars = literal.chars();
                                let Some(ch) = chars.next() else {
                                    return Err(CompileError::Lex(format!(
                                        "empty character literal at byte {start}"
                                    )));
                                };
                                if chars.next().is_some() {
                                    return Err(CompileError::Lex(format!(
                                        "unsupported character literal '#\\{}' at byte {start}",
                                        literal
                                    )));
                                }
                                ch
                            }
                        };
                        tokens.push(Token {
                            kind: TokenKind::Char(ch),
                            span: Span { start, end },
                        });
                        index = end;
                        continue;
                    }
                    other => {
                        return Err(CompileError::Lex(format!(
                            "unsupported reader literal '#{}' at byte {}",
                            other as char, index
                        )));
                    }
                };
                tokens.push(Token { kind, span });
                index += 2;
            }
            b'"' => {
                let start = index;
                index += 1;
                let mut value = String::new();

                while index < bytes.len() {
                    match bytes[index] {
                        b'"' => {
                            index += 1;
                            tokens.push(Token {
                                kind: TokenKind::String(value),
                                span: Span { start, end: index },
                            });
                            break;
                        }
                        b'\\' => {
                            index += 1;
                            if index >= bytes.len() {
                                return Err(CompileError::Lex(
                                    "unterminated string escape".into(),
                                ));
                            }
                            let escaped = match bytes[index] {
                                b'"' => '"',
                                b'\\' => '\\',
                                b'n' => '\n',
                                b'r' => '\r',
                                b't' => '\t',
                                other => {
                                    return Err(CompileError::Lex(format!(
                                        "unsupported string escape '\\{}' at byte {}",
                                        other as char, index
                                    )));
                                }
                            };
                            value.push(escaped);
                            index += 1;
                        }
                        byte => {
                            value.push(byte as char);
                            index += 1;
                        }
                    }
                }

                if !matches!(tokens.last().map(|token| &token.kind), Some(TokenKind::String(_))) {
                    return Err(CompileError::Lex(format!(
                        "unterminated string literal starting at byte {start}"
                    )));
                }
            }
            b'-' | b'0'..=b'9' => {
                let start = index;
                let mut end = index;
                let mut saw_digit = false;

                if bytes[end] == b'-' {
                    end += 1;
                }

                while end < bytes.len() && bytes[end].is_ascii_digit() {
                    saw_digit = true;
                    end += 1;
                }

                if saw_digit && (end == bytes.len() || is_delimiter(bytes[end])) {
                    let value = source[start..end].parse::<i64>().map_err(|error| {
                        CompileError::Lex(format!("invalid integer at byte {start}: {error}"))
                    })?;
                    tokens.push(Token {
                        kind: TokenKind::Integer(value),
                        span: Span { start, end },
                    });
                    index = end;
                } else {
                    let (symbol, end) = consume_symbol(source, index);
                    tokens.push(Token {
                        kind: TokenKind::Symbol(symbol.to_string()),
                        span: Span { start: index, end },
                    });
                    index = end;
                }
            }
            _ => {
                let (symbol, end) = consume_symbol(source, index);
                tokens.push(Token {
                    kind: TokenKind::Symbol(symbol.to_string()),
                    span: Span { start: index, end },
                });
                index = end;
            }
        }
    }

    Ok(tokens)
}

fn consume_symbol(source: &str, start: usize) -> (&str, usize) {
    let bytes = source.as_bytes();
    let mut end = start;
    while end < bytes.len() && !is_delimiter(bytes[end]) {
        end += 1;
    }
    (&source[start..end], end)
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\n' | b'\r' | b'\t' | b'(' | b')' | b';' | b'\'' | b'"'
    )
}
