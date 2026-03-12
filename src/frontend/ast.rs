use crate::span::Span;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    pub forms: Vec<Expr>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExprKind {
    Integer(i64),
    Boolean(bool),
    Char(char),
    String(String),
    Symbol(String),
    List(Vec<Expr>),
    Quote(Box<Expr>),
}
