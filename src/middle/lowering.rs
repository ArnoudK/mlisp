use crate::error::CompileError;
use crate::frontend::ast::{Expr as AstExpr, ExprKind as AstExprKind, Program as AstProgram};
use crate::middle::hir::{Binding, Datum, Expr, ExprKind, Formals, Procedure, Program, TopLevel};

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
        if let AstExprKind::List { items, tail: None } = &form.kind {
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
                    AstExprKind::List { items: signature, tail } => {
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

                        let formals = self.lower_formals(&signature[1..], tail.as_deref())?;
                        let body = self.lower_body(&items[2..])?;
                        return Ok(TopLevel::Procedure(Procedure {
                            name,
                            formals,
                            body,
                        }));
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
            AstExprKind::List { items, tail } => self.lower_list(items, tail.as_deref())?,
        };

        Ok(Expr { kind })
    }

    fn lower_list(&mut self, items: &[AstExpr], tail: Option<&AstExpr>) -> Result<ExprKind, CompileError> {
        if tail.is_some() {
            return Err(CompileError::Lower(
                "dotted lists are only valid in data and formal parameter positions".into(),
            ));
        }
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

                    let formals = match &items[1].kind {
                        AstExprKind::List { items, tail } => {
                            self.lower_formals(items, tail.as_deref())?
                        }
                        AstExprKind::Symbol(symbol) => Formals {
                            required: Vec::new(),
                            rest: Some(symbol.clone()),
                        },
                        _ => {
                            return Err(CompileError::Lower(
                                "lambda parameter list must be a list or symbol".into(),
                            ));
                        }
                    };

                    let body = self.lower_body(&items[2..])?;

                    return Ok(ExprKind::Lambda {
                        formals,
                        body: Box::new(body),
                    });
                }
                "let" => return self.lower_let_form(&items[1..], LetFlavor::Parallel),
                "let*" => return self.lower_let_form(&items[1..], LetFlavor::Sequential),
                "letrec" => return self.lower_let_form(&items[1..], LetFlavor::Recursive),
                "letrec*" => return self.lower_letrec_star_form(&items[1..]),
                "guard" => return self.lower_guard_form(&items[1..]),
                "and" => return self.lower_and_form(&items[1..]),
                "or" => return self.lower_or_form(&items[1..]),
                "when" => return self.lower_when_form(&items[1..], true),
                "unless" => return self.lower_when_form(&items[1..], false),
                "cond" => return self.lower_cond_form(&items[1..]),
                "case" => return self.lower_case_form(&items[1..]),
                "do" => return self.lower_do_form(&items[1..]),
                "delay" => {
                    if items.len() != 2 {
                        return Err(CompileError::Lower(
                            "delay expects exactly one expression".into(),
                        ));
                    }
                    return Ok(ExprKind::Delay(Box::new(self.lower_expr(&items[1])?)));
                }
                "force" => {
                    if items.len() != 2 {
                        return Err(CompileError::Lower(
                            "force expects exactly one expression".into(),
                        ));
                    }
                    return Ok(ExprKind::Force(Box::new(self.lower_expr(&items[1])?)));
                }
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

    fn lower_case_form(&mut self, items: &[AstExpr]) -> Result<ExprKind, CompileError> {
        if items.len() < 2 {
            return Err(CompileError::Lower(
                "case expects a key expression and at least one clause".into(),
            ));
        }
        let key = self.lower_expr(&items[0])?;
        let temp = self.gensym("case");
        Ok(ExprKind::Let {
            bindings: vec![Binding {
                name: temp.clone(),
                value: key,
            }],
            body: Box::new(Expr {
                kind: self.lower_case_clauses(&temp, &items[1..])?,
            }),
        })
    }

    fn lower_do_form(&mut self, items: &[AstExpr]) -> Result<ExprKind, CompileError> {
        if items.len() < 2 {
            return Err(CompileError::Lower(
                "do expects a binding list and a termination clause".into(),
            ));
        }

        let bindings = match &items[0].kind {
            AstExprKind::List { items: bindings, tail: None } => bindings
                .iter()
                .map(|binding| self.lower_do_binding(binding))
                .collect::<Result<Vec<_>, _>>()?,
            _ => {
                return Err(CompileError::Lower(
                    "do bindings must be provided as a list".into(),
                ));
            }
        };

        let (test, result_exprs) = match &items[1].kind {
            AstExprKind::List { items, tail: None } if !items.is_empty() => {
                (self.lower_expr(&items[0])?, &items[1..])
            }
            _ => {
                return Err(CompileError::Lower(
                    "do termination clause must be a non-empty list".into(),
                ));
            }
        };

        let command_exprs = items[2..]
            .iter()
            .map(|expr| self.lower_expr(expr))
            .collect::<Result<Vec<_>, _>>()?;

        let loop_name = self.gensym("do_loop");
        let formals = Formals {
            required: bindings.iter().map(|binding| binding.name.clone()).collect(),
            rest: None,
        };
        let done_expr = if result_exprs.is_empty() {
            unspecified_expr()
        } else {
            self.lower_body(result_exprs)?
        };
        let step_args = bindings
            .iter()
            .map(|binding| {
                binding
                    .step
                    .clone()
                    .unwrap_or_else(|| variable_expr(&binding.name))
            })
            .collect::<Vec<_>>();
        let recur_expr = Expr {
            kind: ExprKind::Call {
                callee: Box::new(variable_expr(&loop_name)),
                args: step_args,
            },
        };
        let mut loop_body_exprs = command_exprs;
        loop_body_exprs.push(recur_expr);
        let loop_body = Expr {
            kind: ExprKind::If {
                condition: Box::new(test),
                then_branch: Box::new(done_expr),
                else_branch: Box::new(if loop_body_exprs.len() == 1 {
                    loop_body_exprs.pop().expect("single do body expr")
                } else {
                    Expr {
                        kind: ExprKind::Begin(loop_body_exprs),
                    }
                }),
            },
        };

        Ok(ExprKind::LetRec {
            bindings: vec![Binding {
                name: loop_name.clone(),
                value: Expr {
                    kind: ExprKind::Lambda {
                        formals,
                        body: Box::new(loop_body),
                    },
                },
            }],
            body: Box::new(Expr {
                kind: ExprKind::Call {
                    callee: Box::new(variable_expr(&loop_name)),
                    args: bindings.into_iter().map(|binding| binding.init).collect(),
                },
            }),
        })
    }

    fn lower_letrec_star_form(&mut self, items: &[AstExpr]) -> Result<ExprKind, CompileError> {
        if items.len() < 2 {
            return Err(CompileError::Lower(
                "letrec* forms require bindings and at least one body expression".into(),
            ));
        }

        let bindings = match &items[0].kind {
            AstExprKind::List { items: bindings, tail: None } => bindings
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

    fn lower_guard_form(&mut self, items: &[AstExpr]) -> Result<ExprKind, CompileError> {
        if items.len() < 2 {
            return Err(CompileError::Lower(
                "guard expects a clause specifier and at least one body expression".into(),
            ));
        }

        let (name, clauses) = match &items[0].kind {
            AstExprKind::List { items, tail: None } => {
                let Some(first) = items.first() else {
                    return Err(CompileError::Lower(
                        "guard clause specifier must start with an exception variable".into(),
                    ));
                };
                let name = match &first.kind {
                    AstExprKind::Symbol(symbol) => symbol.clone(),
                    _ => {
                        return Err(CompileError::Lower(
                            "guard exception variable must be a symbol".into(),
                        ));
                    }
                };
                (name, &items[1..])
            }
            _ => {
                return Err(CompileError::Lower(
                    "guard clause specifier must be a list".into(),
                ));
            }
        };

        let body = self.lower_body(&items[1..])?;
        let handler = Expr {
            kind: self.lower_guard_clauses(clauses, &name)?,
        };
        Ok(ExprKind::Guard {
            name,
            handler: Box::new(handler),
            body: Box::new(body),
        })
    }

    fn lower_cond_clauses(&mut self, clauses: &[AstExpr]) -> Result<ExprKind, CompileError> {
        let Some((first, rest)) = clauses.split_first() else {
            return Ok(ExprKind::Unspecified);
        };
        let AstExprKind::List { items, tail: None } = &first.kind else {
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

    fn lower_guard_clauses(
        &mut self,
        clauses: &[AstExpr],
        exception_name: &str,
    ) -> Result<ExprKind, CompileError> {
        let Some((first, rest)) = clauses.split_first() else {
            return Ok(ExprKind::Call {
                callee: Box::new(variable_expr("raise")),
                args: vec![variable_expr(exception_name)],
            });
        };
        let AstExprKind::List { items, tail: None } = &first.kind else {
            return Err(CompileError::Lower("guard clause must be a list".into()));
        };
        if items.is_empty() {
            return Err(CompileError::Lower("guard clause must not be empty".into()));
        }

        if matches!(&items[0].kind, AstExprKind::Symbol(symbol) if symbol == "else") {
            if !rest.is_empty() {
                return Err(CompileError::Lower(
                    "guard else clause must be the last clause".into(),
                ));
            }
            if items.len() < 2 {
                return Err(CompileError::Lower(
                    "guard else clause expects at least one body expression".into(),
                ));
            }
            return Ok(self.lower_body(&items[1..])?.kind);
        }

        let test = self.lower_expr(&items[0])?;
        let temp = self.gensym("guard");
        let else_expr = Expr {
            kind: self.lower_guard_clauses(rest, exception_name)?,
        };

        let then_expr = if items.len() == 1 {
            variable_expr(&temp)
        } else if matches!(&items[1].kind, AstExprKind::Symbol(symbol) if symbol == "=>") {
            if items.len() != 3 {
                return Err(CompileError::Lower(
                    "guard => clause expects exactly one recipient expression".into(),
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

    fn lower_case_clauses(
        &mut self,
        key_name: &str,
        clauses: &[AstExpr],
    ) -> Result<ExprKind, CompileError> {
        let Some((first, rest)) = clauses.split_first() else {
            return Ok(ExprKind::Unspecified);
        };
        let AstExprKind::List { items, tail: None } = &first.kind else {
            return Err(CompileError::Lower("case clause must be a list".into()));
        };
        if items.is_empty() {
            return Err(CompileError::Lower("case clause must not be empty".into()));
        }

        if matches!(&items[0].kind, AstExprKind::Symbol(symbol) if symbol == "else") {
            if !rest.is_empty() {
                return Err(CompileError::Lower(
                    "case else clause must be the last clause".into(),
                ));
            }
            if items.len() < 2 {
                return Err(CompileError::Lower(
                    "case else clause expects at least one body expression".into(),
                ));
            }
            return Ok(self.lower_body(&items[1..])?.kind);
        }

        let datums = match &items[0].kind {
            AstExprKind::List { items: datums, tail: None } => datums
                .iter()
                .map(|datum| self.lower_datum(datum))
                .collect::<Result<Vec<_>, _>>()?,
            _ => {
                return Err(CompileError::Lower(
                    "case clause datum list must be a proper list".into(),
                ));
            }
        };
        let test_temp = self.gensym("case_match");
        let match_expr = Expr {
            kind: ExprKind::Call {
                callee: Box::new(variable_expr("memv")),
                args: vec![
                    variable_expr(key_name),
                    Expr {
                        kind: ExprKind::Quote(Datum::List {
                            items: datums,
                            tail: None,
                        }),
                    },
                ],
            },
        };
        let then_expr = if items.len() == 1 {
            return Err(CompileError::Lower(
                "case clause expects at least one body expression".into(),
            ));
        } else if matches!(&items[1].kind, AstExprKind::Symbol(symbol) if symbol == "=>") {
            if items.len() != 3 {
                return Err(CompileError::Lower(
                    "case => clause expects exactly one recipient expression".into(),
                ));
            }
            let recipient = self.lower_expr(&items[2])?;
            Expr {
                kind: ExprKind::Call {
                    callee: Box::new(recipient),
                    args: vec![variable_expr(&test_temp)],
                },
            }
        } else {
            self.lower_body(&items[1..])?
        };

        Ok(ExprKind::Let {
            bindings: vec![Binding {
                name: test_temp.clone(),
                value: match_expr,
            }],
            body: Box::new(Expr {
                kind: ExprKind::If {
                    condition: Box::new(variable_expr(&test_temp)),
                    then_branch: Box::new(then_expr),
                    else_branch: Box::new(Expr {
                        kind: self.lower_case_clauses(key_name, rest)?,
                    }),
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
            AstExprKind::List { items, tail } => Ok(Datum::List {
                items: items
                    .iter()
                    .map(|expr| self.lower_datum(expr))
                    .collect::<Result<Vec<_>, _>>()?,
                tail: tail
                    .as_deref()
                    .map(|expr| self.lower_datum(expr).map(Box::new))
                    .transpose()?,
            }),
            AstExprKind::Quote(quoted) => Ok(Datum::List {
                items: vec![Datum::Symbol("quote".into()), self.lower_datum(quoted)?],
                tail: None,
            }),
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
            AstExprKind::List { items: bindings, tail: None } => bindings
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
        let AstExprKind::List { items, tail: None } = &expr.kind else {
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

    fn lower_do_binding(&mut self, expr: &AstExpr) -> Result<DoBinding, CompileError> {
        let AstExprKind::List { items, tail: None } = &expr.kind else {
            return Err(CompileError::Lower(
                "do binding must be a list".into(),
            ));
        };
        if !(2..=3).contains(&items.len()) {
            return Err(CompileError::Lower(
                "do binding must contain a variable, init expression, and optional step expression"
                    .into(),
            ));
        }
        let name = match &items[0].kind {
            AstExprKind::Symbol(symbol) => symbol.clone(),
            _ => {
                return Err(CompileError::Lower(
                    "do binding name must be a symbol".into(),
                ));
            }
        };
        Ok(DoBinding {
            name,
            init: self.lower_expr(&items[1])?,
            step: items.get(2).map(|expr| self.lower_expr(expr)).transpose()?,
        })
    }

    fn lower_formals(
        &mut self,
        required: &[AstExpr],
        tail: Option<&AstExpr>,
    ) -> Result<Formals, CompileError> {
        let required = required
            .iter()
            .map(|param| match &param.kind {
                AstExprKind::Symbol(symbol) => Ok(symbol.clone()),
                _ => Err(CompileError::Lower(
                    "procedure parameters must be symbols".into(),
                )),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let rest = match tail {
            Some(AstExpr {
                kind: AstExprKind::Symbol(symbol),
                ..
            }) => Some(symbol.clone()),
            Some(_) => {
                return Err(CompileError::Lower(
                    "dotted parameter tail must be a symbol".into(),
                ));
            }
            None => None,
        };
        Ok(Formals { required, rest })
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

struct DoBinding {
    name: String,
    init: Expr,
    step: Option<Expr>,
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
