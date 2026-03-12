pub mod ast;
pub mod lexer;
pub mod parser;

use crate::error::CompileError;

pub use ast::{Expr, ExprKind, Program};
pub use parser::Parser;

pub fn parse_program(source: &str) -> Result<Program, CompileError> {
    Parser::new(source)?.parse_program()
}
