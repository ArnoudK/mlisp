use crate::error::CompileError;
use crate::frontend::ast::{Expr as AstExpr, ExprKind as AstExprKind, Program as AstProgram};
use crate::middle::hir::{Binding, Datum, Expr, ExprKind, Procedure, Program, TopLevel};

pub fn lower_program(program: &AstProgram) -> Result<Program, CompileError> {
    let mut lowerer = Lowerer { gensym_counter: 0 };
    let mut items = Vec::with_capacity(program.forms.len());
    for form in &program.forms {
        items.push(lowerer.lower_top_level(form)?);
    }
    Ok(Program { items })
}

struct Lowerer {
    gensym_counter: usize,
}

impl Lowerer {
    fn lower_top_level(&mut self, form: &AstExpr) -> Result<TopLevel, CompileError> {
        if let AstExprKind::List(items) = &form.kind {
            if let Some(AstExpr {
                kind: AstExprKind::Symbol(symbol),
                ..
            }) = items.first()
                && symbol == "define"
            {
                if items.len() < 3 {
                    return Err(CompileError::Lower(
                        "define expects a binding target and at least one body form".into(),
                    ));
                }

                match &items[1].kind {
                    AstExprKind::Symbol(name) => {
                        if items.len() != 3 {
                            return Err(CompileError::Lower(
                                "variable define expects exactly one value expression".into(),
                            ));
                        }

                        let value = self.lower_expr(&items[2])?;
                        return Ok(TopLevel::Definition {
                            name: name.clone(),
                            value,
                        });
                    }
                    AstExprKind::List(signature) => {
                        let Some(name_expr) = signature.first() else {
                            return Err(CompileError::Lower(
                                "procedure define requires a non-empty signature".into(),
                            ));
                        };

                        let name = match &name_expr.kind {
                            AstExprKind::Symbol(name) => name.clone(),
                            _ => {
                                return Err(CompileError::Lower(
                                    "procedure name must be a symbol".into(),
                                ));
                            }
                        };

                        let params = signature[1..]
                            .iter()
                            .map(|param| match &param.kind {
                                AstExprKind::Symbol(symbol) => Ok(symbol.clone()),
                                _ => Err(CompileError::Lower(
                                    "procedure parameters must be symbols".into(),
                                )),
                            })
                            .collect::<Result<Vec<_>, _>>()?;

                        let body = self.lower_body(&items[2..])?;
                        return Ok(TopLevel::Procedure(Procedure { name, params, body }));
                    }
                    _ => {
                        return Err(CompileError::Lower(
                            "define target must be a symbol or parameter list".into(),
                        ));
                    }
                }
            }
        }

        Ok(TopLevel::Expression(self.lower_expr(form)?))
    }

    fn lower_expr(&mut self, expr: &AstExpr) -> Result<Expr, CompileError> {
        let kind = match &expr.kind {
            AstExprKind::Integer(value) => ExprKind::Integer(*value),
            AstExprKind::Boolean(value) => ExprKind::Boolean(*value),
            AstExprKind::Char(value) => ExprKind::Char(*value),
            AstExprKind::String(value) => ExprKind::String(value.clone()),
            AstExprKind::Symbol(symbol) => ExprKind::Variable(symbol.clone()),
            AstExprKind::Quote(quoted) => ExprKind::Quote(self.lower_datum(quoted)?),
            AstExprKind::List(items) => self.lower_list(items)?,
        };

        Ok(Expr { kind })
    }

    fn lower_list(&mut self, items: &[AstExpr]) -> Result<ExprKind, CompileError> {
        let Some(head) = items.first() else {
            return Ok(ExprKind::Begin(Vec::new()));
        };

        if let AstExprKind::Symbol(symbol) = &head.kind {
            match symbol.as_str() {
                "begin" => {
                    let mut exprs = Vec::with_capacity(items.len().saturating_sub(1));
                    for item in &items[1..] {
                        exprs.push(self.lower_expr(item)?);
                    }
                    return Ok(ExprKind::Begin(exprs));
                }
                "if" => {
                    if items.len() != 4 {
                        return Err(CompileError::Lower(
                            "if expects condition, then branch, and else branch".into(),
                        ));
                    }
                    return Ok(ExprKind::If {
                        condition: Box::new(self.lower_expr(&items[1])?),
                        then_branch: Box::new(self.lower_expr(&items[2])?),
                        else_branch: Box::new(self.lower_expr(&items[3])?),
                    });
                }
                "set!" => {
                    if items.len() != 3 {
                        return Err(CompileError::Lower(
                            "set! expects a variable name and a value expression".into(),
                        ));
                    }
                    let name = match &items[1].kind {
                        AstExprKind::Symbol(symbol) => symbol.clone(),
                        _ => {
                            return Err(CompileError::Lower(
                                "set! target must be a symbol".into(),
                            ));
                        }
                    };
                    return Ok(ExprKind::Set {
                        name,
                        value: Box::new(self.lower_expr(&items[2])?),
                    });
                }
                "lambda" => {
                    if items.len() < 3 {
                        return Err(CompileError::Lower(
                            "lambda expects a parameter list and at least one body form".into(),
                        ));
                    }

                    let params = match &items[1].kind {
                        AstExprKind::List(params) => params
                            .iter()
                            .map(|param| match &param.kind {
                                AstExprKind::Symbol(symbol) => Ok(symbol.clone()),
                                _ => Err(CompileError::Lower(
                                    "lambda parameters must be symbols".into(),
                                )),
                            })
                            .collect::<Result<Vec<_>, _>>()?,
                        _ => {
                            return Err(CompileError::Lower(
                                "lambda parameter list must be a list".into(),
                            ));
                        }
                    };

                    let body = self.lower_body(&items[2..])?;

                    return Ok(ExprKind::Lambda {
                        params,
                        body: Box::new(body),
                    });
                }
                "let" => return self.lower_let_form(&items[1..], LetFlavor::Parallel),
                "let*" => return self.lower_let_form(&items[1..], LetFlavor::Sequential),
                "letrec" => return self.lower_let_form(&items[1..], LetFlavor::Recursive),
                "letrec*" => return self.lower_letrec_star_form(&items[1..]),
                "and" => return self.lower_and_form(&items[1..]),
                "or" => return self.lower_or_form(&items[1..]),
                "when" => return self.lower_when_form(&items[1..], true),
                "unless" => return self.lower_when_form(&items[1..], false),
                "cond" => return self.lower_cond_form(&items[1..]),
                _ => {}
            }
        }

        let callee = self.lower_expr(head)?;
        let args = items[1..]
            .iter()
            .map(|expr| self.lower_expr(expr))
            .collect::<Result<Vec<_>, _>>()?;

        Ok(ExprKind::Call {
            callee: Box::new(callee),
            args,
        })
    }

    fn lower_and_form(&mut self, items: &[AstExpr]) -> Result<ExprKind, CompileError> {
        Ok(match items {
            [] => ExprKind::Boolean(true),
            [expr] => self.lower_expr(expr)?.kind,
            [first, rest @ ..] => {
                let value = self.lower_expr(first)?;
                let temp = self.gensym("and");
                ExprKind::Let {
                    bindings: vec![Binding {
                        name: temp.clone(),
                        value,
                    }],
                    body: Box::new(Expr {
                        kind: ExprKind::If {
                            condition: Box::new(variable_expr(&temp)),
                            then_branch: Box::new(Expr {
                                kind: self.lower_and_form(rest)?,
                            }),
                            else_branch: Box::new(variable_expr(&temp)),
                        },
                    }),
                }
            }
        })
    }

    fn lower_or_form(&mut self, items: &[AstExpr]) -> Result<ExprKind, CompileError> {
        Ok(match items {
            [] => ExprKind::Boolean(false),
            [expr] => self.lower_expr(expr)?.kind,
            [first, rest @ ..] => {
                let value = self.lower_expr(first)?;
                let temp = self.gensym("or");
                ExprKind::Let {
                    bindings: vec![Binding {
                        name: temp.clone(),
                        value,
                    }],
                    body: Box::new(Expr {
                        kind: ExprKind::If {
                            condition: Box::new(variable_expr(&temp)),
                            then_branch: Box::new(variable_expr(&temp)),
                            else_branch: Box::new(Expr {
                                kind: self.lower_or_form(rest)?,
                            }),
                        },
                    }),
                }
            }
        })
    }

    fn lower_when_form(
        &mut self,
        items: &[AstExpr],
        when_true: bool,
    ) -> Result<ExprKind, CompileError> {
        if items.len() < 2 {
            return Err(CompileError::Lower(
                if when_true {
                    "when expects a test and at least one body expression"
                } else {
                    "unless expects a test and at least one body expression"
                }
                .into(),
            ));
        }
        let test = self.lower_expr(&items[0])?;
        let body = self.lower_body(&items[1..])?;
        Ok(ExprKind::If {
            condition: Box::new(test),
            then_branch: Box::new(if when_true { body.clone() } else { unspecified_expr() }),
            else_branch: Box::new(if when_true { unspecified_expr() } else { body }),
        })
    }

    fn lower_cond_form(&mut self, clauses: &[AstExpr]) -> Result<ExprKind, CompileError> {
        self.lower_cond_clauses(clauses)
    }

    fn lower_letrec_star_form(&mut self, items: &[AstExpr]) -> Result<ExprKind, CompileError> {
        if items.len() < 2 {
            return Err(CompileError::Lower(
                "letrec* forms require bindings and at least one body expression".into(),
            ));
        }

        let bindings = match &items[0].kind {
            AstExprKind::List(bindings) => bindings
                .iter()
                .map(|binding| self.lower_binding(binding))
                .collect::<Result<Vec<_>, _>>()?,
            _ => {
                return Err(CompileError::Lower(
                    "letrec* bindings must be provided as a list".into(),
                ));
            }
        };

        let body = self.lower_body(&items[1..])?;
        self.nest_letrec_star(&bindings, body)
    }

    fn lower_cond_clauses(&mut self, clauses: &[AstExpr]) -> Result<ExprKind, CompileError> {
        let Some((first, rest)) = clauses.split_first() else {
            return Ok(ExprKind::Unspecified);
        };
        let AstExprKind::List(items) = &first.kind else {
            return Err(CompileError::Lower("cond clause must be a list".into()));
        };
        if items.is_empty() {
            return Err(CompileError::Lower("cond clause must not be empty".into()));
        }

        if matches!(&items[0].kind, AstExprKind::Symbol(symbol) if symbol == "else") {
            if !rest.is_empty() {
                return Err(CompileError::Lower(
                    "cond else clause must be the last clause".into(),
                ));
            }
            if items.len() < 2 {
                return Err(CompileError::Lower(
                    "cond else clause expects at least one body expression".into(),
                ));
            }
            return Ok(self.lower_body(&items[1..])?.kind);
        }

        let test = self.lower_expr(&items[0])?;
        let temp = self.gensym("cond");
        let else_expr = Expr {
            kind: self.lower_cond_clauses(rest)?,
        };

        let then_expr = if items.len() == 1 {
            variable_expr(&temp)
        } else if matches!(&items[1].kind, AstExprKind::Symbol(symbol) if symbol == "=>") {
            if items.len() != 3 {
                return Err(CompileError::Lower(
                    "cond => clause expects exactly one recipient expression".into(),
                ));
            }
            let recipient = self.lower_expr(&items[2])?;
            Expr {
                kind: ExprKind::Call {
                    callee: Box::new(recipient),
                    args: vec![variable_expr(&temp)],
                },
            }
        } else {
            self.lower_body(&items[1..])?
        };

        Ok(ExprKind::Let {
            bindings: vec![Binding {
                name: temp.clone(),
                value: test,
            }],
            body: Box::new(Expr {
                kind: ExprKind::If {
                    condition: Box::new(variable_expr(&temp)),
                    then_branch: Box::new(then_expr),
                    else_branch: Box::new(else_expr),
                },
            }),
        })
    }

    fn lower_body(&mut self, items: &[AstExpr]) -> Result<Expr, CompileError> {
        match items {
            [] => Err(CompileError::Lower(
                "expected at least one expression in body".into(),
            )),
            [expr] => self.lower_expr(expr),
            many => Ok(Expr {
                kind: ExprKind::Begin(
                    many.iter()
                        .map(|expr| self.lower_expr(expr))
                        .collect::<Result<Vec<_>, _>>()?,
                ),
            }),
        }
    }

    fn lower_datum(&mut self, expr: &AstExpr) -> Result<Datum, CompileError> {
        match &expr.kind {
            AstExprKind::Integer(value) => Ok(Datum::Integer(*value)),
            AstExprKind::Boolean(value) => Ok(Datum::Boolean(*value)),
            AstExprKind::Char(value) => Ok(Datum::Char(*value)),
            AstExprKind::String(value) => Ok(Datum::String(value.clone())),
            AstExprKind::Symbol(symbol) => Ok(Datum::Symbol(symbol.clone())),
            AstExprKind::List(items) => items
                .iter()
                .map(|expr| self.lower_datum(expr))
                .collect::<Result<Vec<_>, _>>()
                .map(Datum::List),
            AstExprKind::Quote(quoted) => Ok(Datum::List(vec![
                Datum::Symbol("quote".into()),
                self.lower_datum(quoted)?,
            ])),
        }
    }

    fn lower_let_form(
        &mut self,
        items: &[AstExpr],
        flavor: LetFlavor,
    ) -> Result<ExprKind, CompileError> {
        if items.len() < 2 {
            return Err(CompileError::Lower(
                "let forms require bindings and at least one body expression".into(),
            ));
        }

        let bindings = match &items[0].kind {
            AstExprKind::List(bindings) => bindings
                .iter()
                .map(|binding| self.lower_binding(binding))
                .collect::<Result<Vec<_>, _>>()?,
            _ => {
                return Err(CompileError::Lower(
                    "let bindings must be provided as a list".into(),
                ));
            }
        };
        let body = Box::new(self.lower_body(&items[1..])?);

        Ok(match flavor {
            LetFlavor::Parallel => ExprKind::Let { bindings, body },
            LetFlavor::Sequential => ExprKind::LetStar { bindings, body },
            LetFlavor::Recursive => ExprKind::LetRec { bindings, body },
        })
    }

    fn lower_binding(&mut self, expr: &AstExpr) -> Result<Binding, CompileError> {
        let AstExprKind::List(items) = &expr.kind else {
            return Err(CompileError::Lower(
                "binding must be a two-item list".into(),
            ));
        };

        if items.len() != 2 {
            return Err(CompileError::Lower(
                "binding must contain a name and a value expression".into(),
            ));
        }

        let name = match &items[0].kind {
            AstExprKind::Symbol(symbol) => symbol.clone(),
            _ => {
                return Err(CompileError::Lower("binding name must be a symbol".into()));
            }
        };

        let value = self.lower_expr(&items[1])?;
        Ok(Binding { name, value })
    }

    fn gensym(&mut self, prefix: &str) -> String {
        let name = format!("__{prefix}_{}", self.gensym_counter);
        self.gensym_counter += 1;
        name
    }

    fn nest_letrec_star(&self, bindings: &[Binding], body: Expr) -> Result<ExprKind, CompileError> {
        let Some((first, rest)) = bindings.split_first() else {
            return Ok(body.kind);
        };

        let nested_body = Expr {
            kind: self.nest_letrec_star(rest, body)?,
        };

        Ok(ExprKind::LetRec {
            bindings: vec![first.clone()],
            body: Box::new(nested_body),
        })
    }
}

#[derive(Clone, Copy)]
enum LetFlavor {
    Parallel,
    Sequential,
    Recursive,
}

fn unspecified_expr() -> Expr {
    Expr {
        kind: ExprKind::Unspecified,
    }
}

fn variable_expr(name: &str) -> Expr {
    Expr {
        kind: ExprKind::Variable(name.into()),
    }
}
