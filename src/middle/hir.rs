#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Program {
    pub items: Vec<TopLevel>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TopLevel {
    Definition { name: String, value: Expr },
    Procedure(Procedure),
    Expression(Expr),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Procedure {
    pub name: String,
    pub params: Vec<String>,
    pub body: Expr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expr {
    pub kind: ExprKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExprKind {
    Unspecified,
    Integer(i64),
    Boolean(bool),
    Char(char),
    String(String),
    Variable(String),
    Set {
        name: String,
        value: Box<Expr>,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
    },
    Begin(Vec<Expr>),
    Let {
        bindings: Vec<Binding>,
        body: Box<Expr>,
    },
    LetStar {
        bindings: Vec<Binding>,
        body: Box<Expr>,
    },
    LetRec {
        bindings: Vec<Binding>,
        body: Box<Expr>,
    },
    If {
        condition: Box<Expr>,
        then_branch: Box<Expr>,
        else_branch: Box<Expr>,
    },
    Lambda {
        params: Vec<String>,
        body: Box<Expr>,
    },
    Quote(Datum),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub name: String,
    pub value: Expr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Datum {
    Integer(i64),
    Boolean(bool),
    Char(char),
    String(String),
    Symbol(String),
    List(Vec<Datum>),
}
