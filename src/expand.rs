use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::CompileError;
use crate::frontend::ast::{Expr, ExprKind, Program};
use crate::frontend::parse_program;
use crate::span::Span;

pub fn expand_program(path: &Path, program: &Program) -> Result<Program, CompileError> {
    let base_dir = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let mut expander = Expander::new(base_dir);
    expander.expand_root_program(program)
}

#[derive(Clone)]
struct SyntaxRule {
    pattern: Expr,
    template: Expr,
}

#[derive(Clone)]
struct SyntaxRules {
    literals: HashSet<String>,
    rules: Vec<SyntaxRule>,
    definition_values: HashMap<String, String>,
}

#[derive(Clone, Default)]
struct MatchEnv {
    singles: HashMap<String, Expr>,
    repeats: HashMap<String, Vec<Expr>>,
}

#[derive(Clone)]
struct LoadedLibrary {
    forms: Vec<Expr>,
    value_exports: HashMap<String, String>,
    macro_exports: HashMap<String, SyntaxRules>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExportSpec {
    external: String,
    internal: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ImportSet {
    Library(Vec<String>),
    Only {
        set: Box<ImportSet>,
        names: Vec<String>,
    },
    Except {
        set: Box<ImportSet>,
        names: Vec<String>,
    },
    Prefix {
        set: Box<ImportSet>,
        prefix: String,
    },
    Rename {
        set: Box<ImportSet>,
        renames: Vec<(String, String)>,
    },
}

impl ImportSet {
    fn apply_names(&self, library: &LoadedLibrary) -> Result<HashSet<String>, CompileError> {
        match self {
            Self::Library(_) => Ok(library
                .value_exports
                .keys()
                .chain(library.macro_exports.keys())
                .cloned()
                .collect()),
            Self::Only { set, names } => filter_names(set.apply_names(library)?, names, "only"),
            Self::Except { set, names } => exclude_names(set.apply_names(library)?, names),
            Self::Prefix { set, prefix } => Ok(set
                .apply_names(library)?
                .into_iter()
                .map(|name| format!("{prefix}{name}"))
                .collect()),
            Self::Rename { set, renames } => rename_names(set.apply_names(library)?, renames),
        }
    }

    fn library_name(&self) -> &[String] {
        match self {
            Self::Library(name) => name,
            Self::Only { set, .. }
            | Self::Except { set, .. }
            | Self::Prefix { set, .. }
            | Self::Rename { set, .. } => set.library_name(),
        }
    }

    fn apply_values(
        &self,
        library: &LoadedLibrary,
    ) -> Result<HashMap<String, String>, CompileError> {
        match self {
            Self::Library(_) => Ok(library.value_exports.clone()),
            Self::Only { set, names } => filter_exports(set.apply_values(library)?, names, "only"),
            Self::Except { set, names } => exclude_exports(set.apply_values(library)?, names),
            Self::Prefix { set, prefix } => prefix_exports(set.apply_values(library)?, prefix),
            Self::Rename { set, renames } => rename_exports(set.apply_values(library)?, renames),
        }
    }

    fn apply_macros(
        &self,
        library: &LoadedLibrary,
    ) -> Result<HashMap<String, SyntaxRules>, CompileError> {
        match self {
            Self::Library(_) => Ok(library.macro_exports.clone()),
            Self::Only { set, names } => filter_exports(set.apply_macros(library)?, names, "only"),
            Self::Except { set, names } => exclude_exports(set.apply_macros(library)?, names),
            Self::Prefix { set, prefix } => prefix_exports(set.apply_macros(library)?, prefix),
            Self::Rename { set, renames } => rename_exports(set.apply_macros(library)?, renames),
        }
    }
}

struct Expander {
    base_dir: PathBuf,
    libraries: HashMap<Vec<String>, LoadedLibrary>,
    active_libraries: Vec<Vec<String>>,
    macro_gensym_counter: usize,
}

impl Expander {
    fn new(base_dir: PathBuf) -> Self {
        Self {
            base_dir,
            libraries: HashMap::new(),
            active_libraries: Vec::new(),
            macro_gensym_counter: 0,
        }
    }

    fn expand_root_program(&mut self, program: &Program) -> Result<Program, CompileError> {
        if program.forms.len() == 1
            && let Some((name, declarations)) = parse_define_library_form(&program.forms[0])
        {
            let library = self.load_library_from_declarations(&name, declarations)?;
            return Ok(Program { forms: library.forms });
        }

        let mut macros = HashMap::new();
        let mut values = HashMap::new();
        let mut imported = BTreeSet::new();
        let forms =
            self.expand_top_level_forms(&program.forms, &mut macros, &mut values, &mut imported)?;
        Ok(Program { forms })
    }

    fn expand_top_level_forms(
        &mut self,
        forms: &[Expr],
        macros: &mut HashMap<String, SyntaxRules>,
        values: &mut HashMap<String, String>,
        imported: &mut BTreeSet<Vec<String>>,
    ) -> Result<Vec<Expr>, CompileError> {
        let mut expanded = Vec::new();
        for form in forms {
            if let Some((name, mut rules)) = parse_define_syntax(form, values)? {
                self.materialize_macro_definition_values(&mut rules, values, &mut expanded, form.span);
                macros.insert(name, rules);
                continue;
            }
            if let Some(imports) = parse_import(form)? {
                self.apply_imports(
                    &imports,
                    &mut expanded,
                    macros,
                    values,
                    imported,
                    form.span,
                )?;
                continue;
            }
            if parse_define_library_form(form).is_some() {
                return Err(CompileError::Expand(
                    "define-library is only valid as the whole contents of a library file".into(),
                ));
            }
            let expanded_form = self.expand_expr(form, macros)?;
            if let Some(name) = parse_define_name(&expanded_form)? {
                values.insert(name.clone(), name);
            }
            expanded.push(expanded_form);
        }
        Ok(expanded)
    }

    fn apply_imports(
        &mut self,
        imports: &[ImportSet],
        forms: &mut Vec<Expr>,
        macros: &mut HashMap<String, SyntaxRules>,
        values: &mut HashMap<String, String>,
        imported: &mut BTreeSet<Vec<String>>,
        span: Span,
    ) -> Result<(), CompileError> {
        for import in imports {
            let library_name = import.library_name().to_vec();
            let library = self.load_library(&library_name)?;
            import.apply_names(&library)?;
            if imported.insert(library_name) {
                forms.extend(library.forms.clone());
            }
            for (alias, internal) in import.apply_values(&library)? {
                values.insert(alias.clone(), internal.clone());
                forms.push(make_alias_definition(&alias, &internal, span));
            }
            for (name, rules) in import.apply_macros(&library)? {
                insert_unique_macro(macros, name, rules)?;
            }
        }
        Ok(())
    }

    fn load_library(&mut self, name: &[String]) -> Result<LoadedLibrary, CompileError> {
        if let Some(library) = self.libraries.get(name) {
            return Ok(library.clone());
        }
        if self.active_libraries.iter().any(|entry| entry == name) {
            return Err(CompileError::Expand(format!(
                "cyclic library import detected for ({})",
                name.join(" ")
            )));
        }

        let path = self.resolve_library_path(name)?;
        let source = fs::read_to_string(&path)
            .map_err(|error| CompileError::io(Some(path.clone()), error))?;
        let ast = parse_program(&source)?;
        let (library_name, declarations) = parse_define_library(ast.forms.as_slice())?;
        if library_name != name {
            return Err(CompileError::Expand(format!(
                "library file {} defines ({}) but was imported as ({})",
                path.display(),
                library_name.join(" "),
                name.join(" ")
            )));
        }

        self.active_libraries.push(name.to_vec());
        let library = self.load_library_from_declarations(&library_name, declarations)?;
        self.active_libraries.pop();
        self.libraries.insert(name.to_vec(), library.clone());
        Ok(library)
    }

    fn load_library_from_declarations(
        &mut self,
        name: &[String],
        declarations: &[Expr],
    ) -> Result<LoadedLibrary, CompileError> {
        let rename_map = collect_library_top_level_names(declarations)?
            .into_iter()
            .map(|symbol| {
                let internal = format!("__lib_{}_{}", name.join("_"), symbol);
                (symbol, internal)
            })
            .collect::<HashMap<_, _>>();

        let mut export_specs = Vec::new();
        let mut macros = HashMap::new();
        let mut values = HashMap::new();
        let mut imported = BTreeSet::new();
        let mut forms = Vec::new();

        for declaration in declarations {
            if let Some(exports) = parse_export(declaration)? {
                export_specs.extend(exports);
                continue;
            }
            if let Some(imports) = parse_import(declaration)? {
                self.apply_imports(
                    &imports,
                    &mut forms,
                    &mut macros,
                    &mut values,
                    &mut imported,
                    declaration.span,
                )?;
                continue;
            }
            if let Some(begin_forms) = parse_begin_declaration(declaration) {
                let renamed_begin = begin_forms
                    .iter()
                    .map(|form| rename_library_expr(form, &rename_map))
                    .collect::<Result<Vec<_>, _>>()?;
                forms.extend(self.expand_top_level_forms(
                    &renamed_begin,
                    &mut macros,
                    &mut values,
                    &mut imported,
                )?);
                continue;
            }

            let renamed = rename_library_expr(declaration, &rename_map)?;
            if let Some((macro_name, mut rules)) = parse_define_syntax(&renamed, &values)? {
                self.materialize_macro_definition_values(
                    &mut rules,
                    &values,
                    &mut forms,
                    declaration.span,
                );
                insert_unique_macro(&mut macros, macro_name, rules)?;
                continue;
            }
            if let Some(name) = parse_define_name(&renamed)? {
                values.insert(name.clone(), name);
            }
            forms.push(self.expand_expr(&renamed, &macros)?);
        }

        let mut value_exports = HashMap::new();
        let mut macro_exports = HashMap::new();
        for export in export_specs {
            let internal = rename_map
                .get(&export.internal)
                .cloned()
                .unwrap_or_else(|| export.internal.clone());
            if let Some(rules) = macros.get(&internal) {
                insert_unique_macro(&mut macro_exports, export.external, rules.clone())?;
            } else {
                insert_unique_value(&mut value_exports, export.external, internal)?;
            }
        }

        Ok(LoadedLibrary {
            forms,
            value_exports,
            macro_exports,
        })
    }

    fn resolve_library_path(&self, name: &[String]) -> Result<PathBuf, CompileError> {
        let mut base = self.base_dir.clone();
        for part in name {
            base.push(part);
        }
        let candidates = [
            base.with_extension("sld"),
            base.with_extension("scm"),
            base.join("main.sld"),
            base.join("main.scm"),
        ];
        candidates
            .into_iter()
            .find(|candidate| candidate.exists())
            .ok_or_else(|| {
                CompileError::Expand(format!("could not resolve library ({})", name.join(" ")))
            })
    }

    fn expand_expr(
        &mut self,
        expr: &Expr,
        macros: &HashMap<String, SyntaxRules>,
    ) -> Result<Expr, CompileError> {
        match &expr.kind {
            ExprKind::Integer(_)
            | ExprKind::Boolean(_)
            | ExprKind::Char(_)
            | ExprKind::String(_)
            | ExprKind::Symbol(_) => Ok(expr.clone()),
            ExprKind::Quote(_) => Ok(expr.clone()),
            ExprKind::List { items, tail } => {
                if let Some(head) = items.first()
                    && let ExprKind::Symbol(name) = &head.kind
                {
                    if let Some(rules) = macros.get(name) {
                        let expanded = self.expand_macro_call(rules, expr)?;
                        return self.expand_expr(&expanded, macros);
                    }
                    match name.as_str() {
                        "quote" => return Ok(expr.clone()),
                        "lambda" => return self.expand_lambda(expr, items, tail.as_deref(), macros),
                        "define" => return self.expand_define(expr, items, macros),
                        "define-syntax" | "syntax-rules" | "import" | "define-library" => {
                            return Ok(expr.clone());
                        }
                        "let" | "let*" | "letrec" | "letrec*" => {
                            return self.expand_let_like(expr, name, items, macros);
                        }
                        _ => {}
                    }
                }
                let items = items
                    .iter()
                    .map(|item| self.expand_expr(item, macros))
                    .collect::<Result<Vec<_>, _>>()?;
                let tail = tail
                    .as_deref()
                    .map(|item| self.expand_expr(item, macros).map(Box::new))
                    .transpose()?;
                Ok(Expr {
                    kind: ExprKind::List { items, tail },
                    span: expr.span,
                })
            }
        }
    }

    fn expand_define(
        &mut self,
        expr: &Expr,
        items: &[Expr],
        macros: &HashMap<String, SyntaxRules>,
    ) -> Result<Expr, CompileError> {
        if items.len() < 3 {
            return Ok(expr.clone());
        }
        let mut expanded = vec![items[0].clone(), items[1].clone()];
        expanded.extend(
            items[2..]
                .iter()
                .map(|item| self.expand_expr(item, macros))
                .collect::<Result<Vec<_>, _>>()?,
        );
        Ok(Expr {
            kind: ExprKind::List {
                items: expanded,
                tail: None,
            },
            span: expr.span,
        })
    }

    fn materialize_macro_definition_values(
        &mut self,
        rules: &mut SyntaxRules,
        values: &HashMap<String, String>,
        forms: &mut Vec<Expr>,
        span: Span,
    ) {
        let mut captured = HashMap::new();
        let mut names = values.keys().cloned().collect::<Vec<_>>();
        names.sort();
        for visible in names {
            let internal = values
                .get(&visible)
                .expect("sorted macro definition value name must still exist");
            let alias = self.next_macro_reference_name(&visible);
            forms.push(make_alias_definition(&alias, internal, span));
            captured.insert(visible, alias);
        }
        rules.definition_values = captured;
    }

    fn expand_lambda(
        &mut self,
        expr: &Expr,
        items: &[Expr],
        tail: Option<&Expr>,
        macros: &HashMap<String, SyntaxRules>,
    ) -> Result<Expr, CompileError> {
        if items.len() < 3 {
            return Ok(expr.clone());
        }
        let mut expanded = vec![items[0].clone(), items[1].clone()];
        expanded.extend(
            items[2..]
                .iter()
                .map(|item| self.expand_expr(item, macros))
                .collect::<Result<Vec<_>, _>>()?,
        );
        Ok(Expr {
            kind: ExprKind::List {
                items: expanded,
                tail: tail.cloned().map(Box::new),
            },
            span: expr.span,
        })
    }

    fn expand_let_like(
        &mut self,
        expr: &Expr,
        name: &str,
        items: &[Expr],
        macros: &HashMap<String, SyntaxRules>,
    ) -> Result<Expr, CompileError> {
        if items.len() < 3 {
            return Ok(expr.clone());
        }
        let bindings = match &items[1].kind {
            ExprKind::List { items: bindings, tail: None } => bindings
                .iter()
                .map(|binding| match &binding.kind {
                    ExprKind::List { items, tail: None } if items.len() == 2 => Ok(Expr {
                        kind: ExprKind::List {
                            items: vec![items[0].clone(), self.expand_expr(&items[1], macros)?],
                            tail: None,
                        },
                        span: binding.span,
                    }),
                    _ => Err(CompileError::Expand(format!(
                        "{name} binding must be a two-item list"
                    ))),
                })
                .collect::<Result<Vec<_>, _>>()?,
            _ => return Ok(expr.clone()),
        };
        let mut expanded = vec![
            items[0].clone(),
            Expr {
                kind: ExprKind::List {
                    items: bindings,
                    tail: None,
                },
                span: items[1].span,
            },
        ];
        expanded.extend(
            items[2..]
                .iter()
                .map(|item| self.expand_expr(item, macros))
                .collect::<Result<Vec<_>, _>>()?,
        );
        Ok(Expr {
            kind: ExprKind::List {
                items: expanded,
                tail: None,
            },
            span: expr.span,
        })
    }

    fn expand_macro_call(
        &mut self,
        rules: &SyntaxRules,
        expr: &Expr,
    ) -> Result<Expr, CompileError> {
        for rule in &rules.rules {
            let mut env = MatchEnv::default();
            if match_pattern(&rule.pattern, expr, &rules.literals, &mut env, false)? {
                let scope = HashMap::new();
                return self.expand_template(&rule.template, rules, &env, None, &scope);
            }
        }
        Err(CompileError::Expand(
            "macro expansion failed to match any syntax-rules clause".into(),
        ))
    }

    fn expand_template(
        &mut self,
        template: &Expr,
        rules: &SyntaxRules,
        env: &MatchEnv,
        repeated_index: Option<usize>,
        scope: &HashMap<String, String>,
    ) -> Result<Expr, CompileError> {
        match &template.kind {
            ExprKind::Symbol(symbol) => {
                if let Some(index) = repeated_index
                    && let Some(values) = env.repeats.get(symbol)
                {
                    return values.get(index).cloned().ok_or_else(|| {
                        CompileError::Expand(format!(
                            "missing repeated template binding for '{symbol}'"
                        ))
                    });
                }
                if let Some(value) = env.singles.get(symbol) {
                    return Ok(value.clone());
                }
                if let Some(renamed) = scope.get(symbol) {
                    return Ok(Expr {
                        kind: ExprKind::Symbol(renamed.clone()),
                        span: template.span,
                    });
                }
                if let Some(bound) = rules.definition_values.get(symbol) {
                    return Ok(Expr {
                        kind: ExprKind::Symbol(bound.clone()),
                        span: template.span,
                    });
                }
                Ok(template.clone())
            }
            ExprKind::List { items, tail } => {
                if let Some(head) = items.first()
                    && let Some(keyword) = symbol_value(head)
                {
                    match keyword {
                        "lambda" => {
                            return self.expand_template_lambda(
                                template,
                                items,
                                tail.as_deref(),
                                rules,
                                env,
                                repeated_index,
                                scope,
                            )
                        }
                        "let" | "let*" | "letrec" | "letrec*" => {
                            return self.expand_template_let_like(
                                template,
                                keyword,
                                items,
                                rules,
                                env,
                                repeated_index,
                                scope,
                            )
                        }
                        "set!" => {
                            return self.expand_template_set(
                                template,
                                items,
                                rules,
                                env,
                                repeated_index,
                                scope,
                            )
                        }
                        "define" => {
                            return self.expand_template_define(
                                template,
                                items,
                                rules,
                                env,
                                repeated_index,
                                scope,
                            )
                        }
                        _ => {}
                    }
                }

                let mut expanded_items = Vec::new();
                let mut index = 0usize;
                while index < items.len() {
                    if index + 1 < items.len() && is_symbol(&items[index + 1], "...") {
                        let repeat_count = repeat_count_for_template(&items[index], env)?;
                        for repetition in 0..repeat_count {
                            expanded_items.push(self.expand_template(
                                &items[index],
                                rules,
                                env,
                                Some(repetition),
                                scope,
                            )?);
                        }
                        index += 2;
                        continue;
                    }
                    expanded_items.push(self.expand_template(
                        &items[index],
                        rules,
                        env,
                        repeated_index,
                        scope,
                    )?);
                    index += 1;
                }
                let expanded_tail = tail
                    .as_deref()
                    .map(|expr| {
                        self.expand_template(expr, rules, env, repeated_index, scope)
                            .map(Box::new)
                    })
                    .transpose()?;
                Ok(Expr {
                    kind: ExprKind::List {
                        items: expanded_items,
                        tail: expanded_tail,
                    },
                    span: template.span,
                })
            }
            ExprKind::Quote(inner) => Ok(Expr {
                kind: ExprKind::Quote(Box::new(self.expand_template(
                    inner,
                    rules,
                    env,
                    repeated_index,
                    scope,
                )?)),
                span: template.span,
            }),
            _ => Ok(template.clone()),
        }
    }

    fn expand_template_lambda(
        &mut self,
        template: &Expr,
        items: &[Expr],
        tail: Option<&Expr>,
        rules: &SyntaxRules,
        env: &MatchEnv,
        repeated_index: Option<usize>,
        scope: &HashMap<String, String>,
    ) -> Result<Expr, CompileError> {
        if items.len() < 3 {
            return Ok(template.clone());
        }
        let (formals, body_scope) =
            self.expand_template_formals(&items[1], rules, env, repeated_index, scope)?;
        let mut expanded = vec![items[0].clone(), formals];
        expanded.extend(
            items[2..]
                .iter()
                .map(|item| self.expand_template(item, rules, env, repeated_index, &body_scope))
                .collect::<Result<Vec<_>, _>>()?,
        );
        Ok(Expr {
            kind: ExprKind::List {
                items: expanded,
                tail: tail.cloned().map(Box::new),
            },
            span: template.span,
        })
    }

    fn expand_template_define(
        &mut self,
        template: &Expr,
        items: &[Expr],
        rules: &SyntaxRules,
        env: &MatchEnv,
        repeated_index: Option<usize>,
        scope: &HashMap<String, String>,
    ) -> Result<Expr, CompileError> {
        if items.len() < 3 {
            return Ok(template.clone());
        }
        match &items[1].kind {
            ExprKind::Symbol(_) => {
                let target =
                    self.expand_template_binder(&items[1], rules, env, repeated_index, scope)?;
                let mut items_out = vec![
                    items[0].clone(),
                    target.0,
                    self.expand_template(&items[2], rules, env, repeated_index, scope)?,
                ];
                items_out.extend(
                    items[3..]
                        .iter()
                        .map(|item| self.expand_template(item, rules, env, repeated_index, scope))
                        .collect::<Result<Vec<_>, _>>()?,
                );
                Ok(Expr {
                    kind: ExprKind::List {
                        items: items_out,
                        tail: None,
                    },
                    span: template.span,
                })
            }
            ExprKind::List {
                items: signature,
                tail,
            } if !signature.is_empty() => {
                let name_symbol =
                    self.expand_template_binder(&signature[0], rules, env, repeated_index, scope)?;
                let formals_expr = Expr {
                    kind: ExprKind::List {
                        items: signature[1..].to_vec(),
                        tail: tail.clone(),
                    },
                    span: items[1].span,
                };
                let (renamed_formals, body_scope) =
                    self.expand_template_formals(&formals_expr, rules, env, repeated_index, scope)?;
                let ExprKind::List {
                    items: mut formal_items,
                    tail: formal_tail,
                } = renamed_formals.kind
                else {
                    unreachable!();
                };
                formal_items.insert(0, name_symbol.0);
                let signature_expr = Expr {
                    kind: ExprKind::List {
                        items: formal_items,
                        tail: formal_tail,
                    },
                    span: items[1].span,
                };
                let mut items_out = vec![items[0].clone(), signature_expr];
                items_out.extend(
                    items[2..]
                        .iter()
                        .map(|item| self.expand_template(item, rules, env, repeated_index, &body_scope))
                        .collect::<Result<Vec<_>, _>>()?,
                );
                Ok(Expr {
                    kind: ExprKind::List {
                        items: items_out,
                        tail: None,
                    },
                    span: template.span,
                })
            }
            _ => Ok(template.clone()),
        }
    }

    fn expand_template_set(
        &mut self,
        template: &Expr,
        items: &[Expr],
        rules: &SyntaxRules,
        env: &MatchEnv,
        repeated_index: Option<usize>,
        scope: &HashMap<String, String>,
    ) -> Result<Expr, CompileError> {
        if items.len() != 3 {
            return Ok(template.clone());
        }
        let target = self.expand_template(&items[1], rules, env, repeated_index, scope)?;
        let value = self.expand_template(&items[2], rules, env, repeated_index, scope)?;
        Ok(Expr {
            kind: ExprKind::List {
                items: vec![items[0].clone(), target, value],
                tail: None,
            },
            span: template.span,
        })
    }

    fn expand_template_let_like(
        &mut self,
        template: &Expr,
        name: &str,
        items: &[Expr],
        rules: &SyntaxRules,
        env: &MatchEnv,
        repeated_index: Option<usize>,
        scope: &HashMap<String, String>,
    ) -> Result<Expr, CompileError> {
        if items.len() < 3 {
            return Ok(template.clone());
        }
        let ExprKind::List { items: bindings, tail: None } = &items[1].kind else {
            return Ok(template.clone());
        };

        let mut expanded_bindings = Vec::with_capacity(bindings.len());
        let mut body_scope = scope.clone();
        for binding in bindings {
            let ExprKind::List {
                items: binding_items,
                tail: None,
            } = &binding.kind
            else {
                return Ok(template.clone());
            };
            if binding_items.len() != 2 {
                return Ok(template.clone());
            }
            let init_scope = match name {
                "let" => scope,
                "let*" => &body_scope,
                "letrec" | "letrec*" => &body_scope,
                _ => scope,
            };
            let (binder, binder_name) =
                self.expand_template_binder(&binding_items[0], rules, env, repeated_index, &body_scope)?;
            let init =
                self.expand_template(&binding_items[1], rules, env, repeated_index, init_scope)?;
            body_scope.insert(symbol_name(&binding_items[0], "template binding name")?, binder_name);
            expanded_bindings.push(Expr {
                kind: ExprKind::List {
                    items: vec![binder, init],
                    tail: None,
                },
                span: binding.span,
            });
        }
        let mut items_out = vec![
            items[0].clone(),
            Expr {
                kind: ExprKind::List {
                    items: expanded_bindings,
                    tail: None,
                },
                span: items[1].span,
            },
        ];
        items_out.extend(
            items[2..]
                .iter()
                .map(|item| self.expand_template(item, rules, env, repeated_index, &body_scope))
                .collect::<Result<Vec<_>, _>>()?,
        );
        Ok(Expr {
            kind: ExprKind::List {
                items: items_out,
                tail: None,
            },
            span: template.span,
        })
    }

    fn expand_template_formals(
        &mut self,
        formals: &Expr,
        rules: &SyntaxRules,
        env: &MatchEnv,
        repeated_index: Option<usize>,
        scope: &HashMap<String, String>,
    ) -> Result<(Expr, HashMap<String, String>), CompileError> {
        match &formals.kind {
            ExprKind::Symbol(_) => {
                let (binder, renamed) =
                    self.expand_template_binder(formals, rules, env, repeated_index, scope)?;
                let original = symbol_name(formals, "template formal")?;
                let mut next_scope = scope.clone();
                next_scope.insert(original, renamed);
                Ok((binder, next_scope))
            }
            ExprKind::List { items, tail } => {
                let mut next_scope = scope.clone();
                let mut out_items = Vec::with_capacity(items.len());
                for item in items {
                    let original = symbol_name(item, "template formal")?;
                    let (binder, renamed) =
                        self.expand_template_binder(item, rules, env, repeated_index, &next_scope)?;
                    next_scope.insert(original, renamed);
                    out_items.push(binder);
                }
                let out_tail = if let Some(tail) = tail {
                    let original = symbol_name(tail, "template rest formal")?;
                    let (binder, renamed) =
                        self.expand_template_binder(tail, rules, env, repeated_index, &next_scope)?;
                    next_scope.insert(original, renamed);
                    Some(Box::new(binder))
                } else {
                    None
                };
                Ok((
                    Expr {
                        kind: ExprKind::List {
                            items: out_items,
                            tail: out_tail,
                        },
                        span: formals.span,
                    },
                    next_scope,
                ))
            }
            _ => Err(CompileError::Expand(
                "macro-generated formals must be identifiers or a proper parameter list".into(),
            )),
        }
    }

    fn expand_template_binder(
        &mut self,
        binder: &Expr,
        rules: &SyntaxRules,
        env: &MatchEnv,
        repeated_index: Option<usize>,
        scope: &HashMap<String, String>,
    ) -> Result<(Expr, String), CompileError> {
        let expanded = self.expand_template(binder, rules, env, repeated_index, scope)?;
        let name = symbol_name(&expanded, "macro-generated binder")?;
        if is_pattern_variable_name(&name, env) {
            return Ok((expanded, name));
        }
        let fresh = self.next_macro_gensym(&name);
        Ok((
            Expr {
                kind: ExprKind::Symbol(fresh.clone()),
                span: expanded.span,
            },
            fresh,
        ))
    }

    fn next_macro_gensym(&mut self, base: &str) -> String {
        let fresh = format!("__macro_{}_{}", base, self.macro_gensym_counter);
        self.macro_gensym_counter += 1;
        fresh
    }

    fn next_macro_reference_name(&mut self, base: &str) -> String {
        let fresh = format!("__macro_ref_{}_{}", base, self.macro_gensym_counter);
        self.macro_gensym_counter += 1;
        fresh
    }
}

fn parse_define_library(forms: &[Expr]) -> Result<(Vec<String>, &[Expr]), CompileError> {
    if forms.len() != 1 {
        return Err(CompileError::Expand(
            "library file must contain exactly one define-library form".into(),
        ));
    }
    let Some((name, declarations)) = parse_define_library_form(&forms[0]) else {
        return Err(CompileError::Expand(
            "library file must contain a define-library form".into(),
        ));
    };
    Ok((name, declarations))
}

fn parse_define_library_form(form: &Expr) -> Option<(Vec<String>, &[Expr])> {
    let ExprKind::List { items, tail: None } = &form.kind else {
        return None;
    };
    if items.len() < 2 || !is_symbol(&items[0], "define-library") {
        return None;
    }
    let name = parse_library_name_expr(&items[1]).ok()?;
    Some((name, &items[2..]))
}

fn parse_export(form: &Expr) -> Result<Option<Vec<ExportSpec>>, CompileError> {
    let ExprKind::List { items, tail: None } = &form.kind else {
        return Ok(None);
    };
    if items.is_empty() || !is_symbol(&items[0], "export") {
        return Ok(None);
    }
    let mut names = Vec::with_capacity(items.len().saturating_sub(1));
    for item in &items[1..] {
        match &item.kind {
            ExprKind::Symbol(symbol) => names.push(ExportSpec {
                external: symbol.clone(),
                internal: symbol.clone(),
            }),
            ExprKind::List { items, tail: None }
                if items.len() == 3 && is_symbol(&items[0], "rename") =>
            {
                let internal = symbol_name(&items[1], "export rename source")?;
                let external = symbol_name(&items[2], "export rename target")?;
                names.push(ExportSpec { external, internal });
            }
            _ => {
                return Err(CompileError::Expand(
                    "export entries must be identifiers or (rename internal external)".into(),
                ))
            }
        }
    }
    Ok(Some(names))
}

fn parse_import(form: &Expr) -> Result<Option<Vec<ImportSet>>, CompileError> {
    let ExprKind::List { items, tail: None } = &form.kind else {
        return Ok(None);
    };
    if items.is_empty() || !is_symbol(&items[0], "import") {
        return Ok(None);
    }
    let mut imports = Vec::new();
    for item in &items[1..] {
        imports.push(parse_import_set(item)?);
    }
    Ok(Some(imports))
}

fn parse_import_set(expr: &Expr) -> Result<ImportSet, CompileError> {
    let ExprKind::List { items, tail: None } = &expr.kind else {
        return Err(CompileError::Expand(
            "import entries must be library names or import sets".into(),
        ));
    };
    if items.is_empty() {
        return Err(CompileError::Expand("import set may not be empty".into()));
    }
    if let Some(keyword) = symbol_value(&items[0]) {
        match keyword {
            "only" => {
                if items.len() < 3 {
                    return Err(CompileError::Expand(
                        "only import set requires a base set and at least one identifier".into(),
                    ));
                }
                let set = Box::new(parse_import_set(&items[1])?);
                let names = parse_identifier_list(&items[2..], "only import set identifiers")?;
                return Ok(ImportSet::Only { set, names });
            }
            "except" => {
                if items.len() < 3 {
                    return Err(CompileError::Expand(
                        "except import set requires a base set and at least one identifier".into(),
                    ));
                }
                let set = Box::new(parse_import_set(&items[1])?);
                let names = parse_identifier_list(&items[2..], "except import set identifiers")?;
                return Ok(ImportSet::Except { set, names });
            }
            "prefix" => {
                if items.len() != 3 {
                    return Err(CompileError::Expand(
                        "prefix import set requires a base set and a prefix identifier".into(),
                    ));
                }
                let set = Box::new(parse_import_set(&items[1])?);
                let prefix = symbol_name(&items[2], "prefix import set prefix")?;
                return Ok(ImportSet::Prefix { set, prefix });
            }
            "rename" => {
                if items.len() < 3 {
                    return Err(CompileError::Expand(
                        "rename import set requires a base set and at least one rename clause".into(),
                    ));
                }
                let set = Box::new(parse_import_set(&items[1])?);
                let renames = items[2..]
                    .iter()
                    .map(parse_rename_pair)
                    .collect::<Result<Vec<_>, _>>()?;
                return Ok(ImportSet::Rename { set, renames });
            }
            _ => {}
        }
    }
    Ok(ImportSet::Library(parse_library_name_expr(expr)?))
}

fn parse_begin_declaration(form: &Expr) -> Option<&[Expr]> {
    let ExprKind::List { items, tail: None } = &form.kind else {
        return None;
    };
    if items.is_empty() || !is_symbol(&items[0], "begin") {
        return None;
    }
    Some(&items[1..])
}

fn parse_define_syntax(
    form: &Expr,
    values: &HashMap<String, String>,
) -> Result<Option<(String, SyntaxRules)>, CompileError> {
    let ExprKind::List { items, tail: None } = &form.kind else {
        return Ok(None);
    };
    if items.len() != 3 || !is_symbol(&items[0], "define-syntax") {
        return Ok(None);
    }
    let name = symbol_name(&items[1], "define-syntax name")?;
    Ok(Some((name, parse_syntax_rules(&items[2], values)?)))
}

fn parse_syntax_rules(
    expr: &Expr,
    values: &HashMap<String, String>,
) -> Result<SyntaxRules, CompileError> {
    let ExprKind::List { items, tail: None } = &expr.kind else {
        return Err(CompileError::Expand(
            "syntax-rules transformer must be a list".into(),
        ));
    };
    if items.len() < 3 || !is_symbol(&items[0], "syntax-rules") {
        return Err(CompileError::Expand(
            "define-syntax requires a syntax-rules transformer".into(),
        ));
    }
    let ExprKind::List {
        items: literal_items,
        tail: None,
    } = &items[1].kind
    else {
        return Err(CompileError::Expand(
            "syntax-rules literal list must be a proper list".into(),
        ));
    };
    let mut literals = HashSet::new();
    for item in literal_items {
        literals.insert(symbol_name(item, "syntax-rules literal")?);
    }
    let rules = items[2..]
        .iter()
        .map(|rule| {
            let ExprKind::List { items, tail: None } = &rule.kind else {
                return Err(CompileError::Expand(
                    "syntax-rules clause must be a two-item list".into(),
                ));
            };
            if items.len() != 2 {
                return Err(CompileError::Expand(
                    "syntax-rules clause must contain a pattern and template".into(),
                ));
            }
            Ok(SyntaxRule {
                pattern: items[0].clone(),
                template: items[1].clone(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(SyntaxRules {
        literals,
        rules,
        definition_values: values.clone(),
    })
}

fn collect_library_top_level_names(declarations: &[Expr]) -> Result<HashSet<String>, CompileError> {
    let mut names = HashSet::new();
    for declaration in declarations {
        collect_top_level_names_in_form(declaration, &mut names)?;
    }
    Ok(names)
}

fn collect_top_level_names_in_form(
    form: &Expr,
    names: &mut HashSet<String>,
) -> Result<(), CompileError> {
    if let Some(begin_forms) = parse_begin_declaration(form) {
        for begin_form in begin_forms {
            collect_top_level_names_in_form(begin_form, names)?;
        }
        return Ok(());
    }
    if parse_export(form)?.is_some() || parse_import(form)?.is_some() {
        return Ok(());
    }
    let macro_values = HashMap::new();
    if let Some((name, _)) = parse_define_syntax(form, &macro_values)? {
        names.insert(name);
        return Ok(());
    }
    if let Some(name) = parse_define_name(form)? {
        names.insert(name);
    }
    Ok(())
}

fn parse_define_name(form: &Expr) -> Result<Option<String>, CompileError> {
    let ExprKind::List { items, tail: None } = &form.kind else {
        return Ok(None);
    };
    if items.len() < 3 || !is_symbol(&items[0], "define") {
        return Ok(None);
    }
    match &items[1].kind {
        ExprKind::Symbol(symbol) => Ok(Some(symbol.clone())),
        ExprKind::List { items: head, .. } if !head.is_empty() => {
            Ok(Some(symbol_name(&head[0], "procedure definition name")?))
        }
        _ => Err(CompileError::Expand(
            "define target must be an identifier or parameter list".into(),
        )),
    }
}

fn rename_library_expr(
    expr: &Expr,
    rename_map: &HashMap<String, String>,
) -> Result<Expr, CompileError> {
    let bound = HashSet::new();
    rename_expr(expr, rename_map, &bound)
}

fn rename_expr(
    expr: &Expr,
    rename_map: &HashMap<String, String>,
    bound: &HashSet<String>,
) -> Result<Expr, CompileError> {
    match &expr.kind {
        ExprKind::Symbol(symbol) => Ok(Expr {
            kind: ExprKind::Symbol(rename_symbol(symbol, rename_map, bound)),
            span: expr.span,
        }),
        ExprKind::Quote(_) | ExprKind::Integer(_) | ExprKind::Boolean(_) | ExprKind::Char(_)
        | ExprKind::String(_) => Ok(expr.clone()),
        ExprKind::List { items, tail } => {
            if let Some(head) = items.first()
                && let Some(keyword) = symbol_value(head)
            {
                match keyword {
                    "lambda" => return rename_lambda_form(expr, items, tail.as_deref(), rename_map, bound),
                    "define" => return rename_define_form(expr, items, rename_map, bound),
                    "let" | "let*" | "letrec" | "letrec*" => {
                        return rename_let_like(expr, keyword, items, rename_map, bound)
                    }
                    "set!" => return rename_set_form(expr, items, rename_map, bound),
                    "quote" | "import" | "export" | "define-library" => return Ok(expr.clone()),
                    _ => {}
                }
            }
            Ok(Expr {
                kind: ExprKind::List {
                    items: items
                        .iter()
                        .map(|item| rename_expr(item, rename_map, bound))
                        .collect::<Result<Vec<_>, _>>()?,
                    tail: tail
                        .as_deref()
                        .map(|item| rename_expr(item, rename_map, bound).map(Box::new))
                        .transpose()?,
                },
                span: expr.span,
            })
        }
    }
}

fn rename_lambda_form(
    expr: &Expr,
    items: &[Expr],
    tail: Option<&Expr>,
    rename_map: &HashMap<String, String>,
    bound: &HashSet<String>,
) -> Result<Expr, CompileError> {
    if items.len() < 3 {
        return Ok(expr.clone());
    }
    let mut body_bound = bound.clone();
    bind_formals(&items[1], &mut body_bound)?;
    let mut renamed = vec![items[0].clone(), items[1].clone()];
    renamed.extend(
        items[2..]
            .iter()
            .map(|item| rename_expr(item, rename_map, &body_bound))
            .collect::<Result<Vec<_>, _>>()?,
    );
    Ok(Expr {
        kind: ExprKind::List {
            items: renamed,
            tail: tail.cloned().map(Box::new),
        },
        span: expr.span,
    })
}

fn rename_define_form(
    expr: &Expr,
    items: &[Expr],
    rename_map: &HashMap<String, String>,
    bound: &HashSet<String>,
) -> Result<Expr, CompileError> {
    if items.len() < 3 {
        return Ok(expr.clone());
    }
    match &items[1].kind {
        ExprKind::Symbol(symbol) => Ok(Expr {
            kind: ExprKind::List {
                items: vec![
                    items[0].clone(),
                    Expr {
                        kind: ExprKind::Symbol(rename_symbol(symbol, rename_map, bound)),
                        span: items[1].span,
                    },
                    rename_expr(&items[2], rename_map, bound)?,
                ]
                .into_iter()
                .chain(
                    items[3..]
                        .iter()
                        .map(|item| rename_expr(item, rename_map, bound))
                        .collect::<Result<Vec<_>, _>>()?,
                )
                .collect(),
                tail: None,
            },
            span: expr.span,
        }),
        ExprKind::List { items: signature, tail } if !signature.is_empty() => {
            let mut body_bound = bound.clone();
            bind_signature_formals(signature, tail.as_deref(), &mut body_bound)?;
            let mut renamed_signature = vec![Expr {
                kind: ExprKind::Symbol(rename_symbol(
                    &symbol_name(&signature[0], "procedure definition name")?,
                    rename_map,
                    bound,
                )),
                span: signature[0].span,
            }];
            renamed_signature.extend(signature[1..].iter().cloned());
            Ok(Expr {
                kind: ExprKind::List {
                    items: vec![
                        items[0].clone(),
                        Expr {
                            kind: ExprKind::List {
                                items: renamed_signature,
                                tail: tail.clone(),
                            },
                            span: items[1].span,
                        },
                    ]
                    .into_iter()
                    .chain(
                        items[2..]
                            .iter()
                            .map(|item| rename_expr(item, rename_map, &body_bound))
                            .collect::<Result<Vec<_>, _>>()?,
                    )
                    .collect(),
                    tail: None,
                },
                span: expr.span,
            })
        }
        _ => Err(CompileError::Expand(
            "define target must be an identifier or parameter list".into(),
        )),
    }
}

fn rename_let_like(
    expr: &Expr,
    name: &str,
    items: &[Expr],
    rename_map: &HashMap<String, String>,
    bound: &HashSet<String>,
) -> Result<Expr, CompileError> {
    if items.len() < 3 {
        return Ok(expr.clone());
    }
    let ExprKind::List {
        items: bindings,
        tail: None,
    } = &items[1].kind
    else {
        return Ok(expr.clone());
    };

    let mut renamed_bindings = Vec::with_capacity(bindings.len());
    let mut body_bound = bound.clone();
    for binding in bindings {
        let ExprKind::List {
            items: binding_items,
            tail: None,
        } = &binding.kind
        else {
            return Err(CompileError::Expand(format!(
                "{name} binding must be a two-item list"
            )));
        };
        if binding_items.len() != 2 {
            return Err(CompileError::Expand(format!(
                "{name} binding must be a two-item list"
            )));
        }
        let binding_name = symbol_name(&binding_items[0], &format!("{name} binding name"))?;
        let init_bound = match name {
            "let" => bound.clone(),
            "let*" => body_bound.clone(),
            "letrec" | "letrec*" => {
                let mut recursive = body_bound.clone();
                for future in bindings {
                    if let ExprKind::List { items, tail: None } = &future.kind
                        && items.len() == 2
                        && let Some(symbol) = symbol_value(&items[0])
                    {
                        recursive.insert(symbol.to_string());
                    }
                }
                recursive
            }
            _ => bound.clone(),
        };
        let renamed_init = rename_expr(&binding_items[1], rename_map, &init_bound)?;
        body_bound.insert(binding_name.clone());
        renamed_bindings.push(Expr {
            kind: ExprKind::List {
                items: vec![binding_items[0].clone(), renamed_init],
                tail: None,
            },
            span: binding.span,
        });
    }
    let mut renamed_items = vec![
        items[0].clone(),
        Expr {
            kind: ExprKind::List {
                items: renamed_bindings,
                tail: None,
            },
            span: items[1].span,
        },
    ];
    renamed_items.extend(
        items[2..]
            .iter()
            .map(|item| rename_expr(item, rename_map, &body_bound))
            .collect::<Result<Vec<_>, _>>()?,
    );
    Ok(Expr {
        kind: ExprKind::List {
            items: renamed_items,
            tail: None,
        },
        span: expr.span,
    })
}

fn rename_set_form(
    expr: &Expr,
    items: &[Expr],
    rename_map: &HashMap<String, String>,
    bound: &HashSet<String>,
) -> Result<Expr, CompileError> {
    if items.len() != 3 {
        return Ok(expr.clone());
    }
    let target = symbol_name(&items[1], "set! target")?;
    Ok(Expr {
        kind: ExprKind::List {
            items: vec![
                items[0].clone(),
                Expr {
                    kind: ExprKind::Symbol(rename_symbol(&target, rename_map, bound)),
                    span: items[1].span,
                },
                rename_expr(&items[2], rename_map, bound)?,
            ],
            tail: None,
        },
        span: expr.span,
    })
}

fn bind_formals(formals: &Expr, bound: &mut HashSet<String>) -> Result<(), CompileError> {
    match &formals.kind {
        ExprKind::Symbol(symbol) => {
            bound.insert(symbol.clone());
            Ok(())
        }
        ExprKind::List { items, tail } => {
            for item in items {
                bound.insert(symbol_name(item, "lambda parameter")?);
            }
            if let Some(tail) = tail {
                bound.insert(symbol_name(tail, "lambda rest parameter")?);
            }
            Ok(())
        }
        _ => Err(CompileError::Expand(
            "lambda formals must be an identifier or parameter list".into(),
        )),
    }
}

fn bind_signature_formals(
    items: &[Expr],
    tail: Option<&Expr>,
    bound: &mut HashSet<String>,
) -> Result<(), CompileError> {
    for item in &items[1..] {
        bound.insert(symbol_name(item, "procedure parameter")?);
    }
    if let Some(tail) = tail {
        bound.insert(symbol_name(tail, "procedure rest parameter")?);
    }
    Ok(())
}

fn rename_symbol(
    symbol: &str,
    rename_map: &HashMap<String, String>,
    bound: &HashSet<String>,
) -> String {
    if bound.contains(symbol) {
        return symbol.to_string();
    }
    rename_map
        .get(symbol)
        .cloned()
        .unwrap_or_else(|| symbol.to_string())
}

fn parse_library_name_expr(expr: &Expr) -> Result<Vec<String>, CompileError> {
    let ExprKind::List { items, tail: None } = &expr.kind else {
        return Err(CompileError::Expand(
            "library names must be proper lists of identifiers".into(),
        ));
    };
    parse_identifier_list(items, "library name")
}

fn parse_identifier_list(items: &[Expr], context: &str) -> Result<Vec<String>, CompileError> {
    items.iter().map(|item| symbol_name(item, context)).collect()
}

fn parse_rename_pair(expr: &Expr) -> Result<(String, String), CompileError> {
    let ExprKind::List { items, tail: None } = &expr.kind else {
        return Err(CompileError::Expand(
            "rename entries must be two-item lists".into(),
        ));
    };
    if items.len() != 2 {
        return Err(CompileError::Expand(
            "rename entries must be two-item lists".into(),
        ));
    }
    Ok((
        symbol_name(&items[0], "rename source")?,
        symbol_name(&items[1], "rename target")?,
    ))
}

fn symbol_name(expr: &Expr, context: &str) -> Result<String, CompileError> {
    match &expr.kind {
        ExprKind::Symbol(symbol) => Ok(symbol.clone()),
        _ => Err(CompileError::Expand(format!("{context} must be an identifier"))),
    }
}

fn symbol_value(expr: &Expr) -> Option<&str> {
    match &expr.kind {
        ExprKind::Symbol(symbol) => Some(symbol),
        _ => None,
    }
}

fn insert_unique_macro(
    macros: &mut HashMap<String, SyntaxRules>,
    name: String,
    rules: SyntaxRules,
) -> Result<(), CompileError> {
    if macros.insert(name.clone(), rules).is_some() {
        return Err(CompileError::Expand(format!(
            "duplicate macro binding for '{name}'"
        )));
    }
    Ok(())
}

fn insert_unique_value(
    values: &mut HashMap<String, String>,
    name: String,
    internal: String,
) -> Result<(), CompileError> {
    if values.insert(name.clone(), internal).is_some() {
        return Err(CompileError::Expand(format!(
            "duplicate value binding for '{name}'"
        )));
    }
    Ok(())
}

fn filter_exports<T: Clone>(
    exports: HashMap<String, T>,
    names: &[String],
    mode: &str,
) -> Result<HashMap<String, T>, CompileError> {
    let mut filtered = HashMap::new();
    for name in names {
        if let Some(value) = exports.get(name).cloned() {
            if filtered.insert(name.clone(), value).is_some() {
                return Err(CompileError::Expand(format!(
                    "duplicate binding for '{name}' in {mode} import"
                )));
            }
        }
    }
    Ok(filtered)
}

fn exclude_exports<T>(mut exports: HashMap<String, T>, names: &[String]) -> Result<HashMap<String, T>, CompileError> {
    for name in names {
        exports.remove(name);
    }
    Ok(exports)
}

fn prefix_exports<T>(exports: HashMap<String, T>, prefix: &str) -> Result<HashMap<String, T>, CompileError> {
    let mut prefixed = HashMap::new();
    for (name, value) in exports {
        let aliased = format!("{prefix}{name}");
        if prefixed.insert(aliased.clone(), value).is_some() {
            return Err(CompileError::Expand(format!(
                "duplicate binding for '{aliased}' after prefix import"
            )));
        }
    }
    Ok(prefixed)
}

fn rename_exports<T: Clone>(
    exports: HashMap<String, T>,
    renames: &[(String, String)],
) -> Result<HashMap<String, T>, CompileError> {
    let mut renamed = HashMap::new();
    let mut replacements = HashMap::new();
    for (from, to) in renames {
        if let Some(value) = exports.get(from).cloned()
            && replacements
                .insert(from.clone(), (to.clone(), value))
                .is_some()
        {
            return Err(CompileError::Expand(format!(
                "duplicate rename source '{from}'"
            )));
        }
    }
    for (name, value) in exports {
        if let Some((target, replacement)) = replacements.get(&name) {
            if renamed.insert(target.clone(), replacement.clone()).is_some() {
                return Err(CompileError::Expand(format!(
                    "duplicate binding for '{}' after rename import",
                    target
                )));
            }
        } else if renamed.insert(name.clone(), value).is_some() {
            return Err(CompileError::Expand(format!(
                "duplicate binding for '{name}' after rename import"
            )));
        }
    }
    Ok(renamed)
}

fn filter_names(
    names: HashSet<String>,
    only: &[String],
    mode: &str,
) -> Result<HashSet<String>, CompileError> {
    let mut filtered = HashSet::new();
    for name in only {
        if !names.contains(name) {
            return Err(CompileError::Expand(format!(
                "{mode} import references unknown export '{name}'"
            )));
        }
        filtered.insert(name.clone());
    }
    Ok(filtered)
}

fn exclude_names(
    mut names: HashSet<String>,
    excluded: &[String],
) -> Result<HashSet<String>, CompileError> {
    for name in excluded {
        if !names.remove(name) {
            return Err(CompileError::Expand(format!(
                "except import references unknown export '{name}'"
            )));
        }
    }
    Ok(names)
}

fn rename_names(
    names: HashSet<String>,
    renames: &[(String, String)],
) -> Result<HashSet<String>, CompileError> {
    let mut replacements = HashMap::new();
    for (from, to) in renames {
        if !names.contains(from) {
            return Err(CompileError::Expand(format!(
                "rename import references unknown export '{from}'"
            )));
        }
        if replacements.insert(from.clone(), to.clone()).is_some() {
            return Err(CompileError::Expand(format!(
                "duplicate rename source '{from}'"
            )));
        }
    }
    let mut renamed = HashSet::new();
    for name in names {
        let final_name = replacements.get(&name).cloned().unwrap_or(name);
        if !renamed.insert(final_name.clone()) {
            return Err(CompileError::Expand(format!(
                "duplicate binding for '{final_name}' after rename import"
            )));
        }
    }
    Ok(renamed)
}

fn make_alias_definition(alias: &str, internal: &str, span: Span) -> Expr {
    Expr {
        kind: ExprKind::List {
            items: vec![
                symbol_expr("define", span),
                symbol_expr(alias, span),
                symbol_expr(internal, span),
            ],
            tail: None,
        },
        span,
    }
}

fn symbol_expr(symbol: &str, span: Span) -> Expr {
    Expr {
        kind: ExprKind::Symbol(symbol.to_string()),
        span,
    }
}

fn match_pattern(
    pattern: &Expr,
    expr: &Expr,
    literals: &HashSet<String>,
    env: &mut MatchEnv,
    repeated: bool,
) -> Result<bool, CompileError> {
    match (&pattern.kind, &expr.kind) {
        (ExprKind::Integer(lhs), ExprKind::Integer(rhs)) => Ok(lhs == rhs),
        (ExprKind::Boolean(lhs), ExprKind::Boolean(rhs)) => Ok(lhs == rhs),
        (ExprKind::Char(lhs), ExprKind::Char(rhs)) => Ok(lhs == rhs),
        (ExprKind::String(lhs), ExprKind::String(rhs)) => Ok(lhs == rhs),
        (ExprKind::Quote(lhs), ExprKind::Quote(rhs)) => {
            match_pattern(lhs, rhs, literals, env, repeated)
        }
        (ExprKind::Symbol(symbol), _) => {
            match_identifier_pattern(symbol, expr, literals, env, repeated)
        }
        (
            ExprKind::List {
                items: pattern_items,
                tail: pattern_tail,
            },
            ExprKind::List {
                items: expr_items,
                tail: expr_tail,
            },
        ) => match_list_pattern(
            pattern_items,
            pattern_tail.as_deref(),
            expr_items,
            expr_tail.as_deref(),
            literals,
            env,
            repeated,
        ),
        _ => Ok(false),
    }
}

fn match_identifier_pattern(
    symbol: &str,
    expr: &Expr,
    literals: &HashSet<String>,
    env: &mut MatchEnv,
    repeated: bool,
) -> Result<bool, CompileError> {
    if symbol == "_" {
        return Ok(true);
    }
    if literals.contains(symbol) {
        return Ok(matches!(&expr.kind, ExprKind::Symbol(candidate) if candidate == symbol));
    }
    if repeated {
        env.repeats
            .entry(symbol.to_string())
            .or_default()
            .push(expr.clone());
        return Ok(true);
    }
    match env.singles.get(symbol) {
        Some(bound) => Ok(bound == expr),
        None => {
            env.singles.insert(symbol.to_string(), expr.clone());
            Ok(true)
        }
    }
}

fn match_list_pattern(
    pattern_items: &[Expr],
    pattern_tail: Option<&Expr>,
    expr_items: &[Expr],
    expr_tail: Option<&Expr>,
    literals: &HashSet<String>,
    env: &mut MatchEnv,
    repeated: bool,
) -> Result<bool, CompileError> {
    match_list_segments(
        pattern_items,
        pattern_tail,
        expr_items,
        expr_tail,
        literals,
        env,
        repeated,
    )
}

fn match_list_segments(
    pattern_items: &[Expr],
    pattern_tail: Option<&Expr>,
    expr_items: &[Expr],
    expr_tail: Option<&Expr>,
    literals: &HashSet<String>,
    env: &mut MatchEnv,
    repeated: bool,
) -> Result<bool, CompileError> {
    if pattern_items.is_empty() {
        return match_list_tail(pattern_tail, expr_items, expr_tail, literals, env, repeated);
    }
    if pattern_items.len() >= 2 && is_symbol(&pattern_items[1], "...") {
        for consumed in 0..=expr_items.len() {
            let mut trial_env = env.clone();
            let mut ok = true;
            for expr in &expr_items[..consumed] {
                if !match_pattern(&pattern_items[0], expr, literals, &mut trial_env, true)? {
                    ok = false;
                    break;
                }
            }
            if !ok {
                continue;
            }
            if match_list_segments(
                &pattern_items[2..],
                pattern_tail,
                &expr_items[consumed..],
                expr_tail,
                literals,
                &mut trial_env,
                repeated,
            )? {
                *env = trial_env;
                return Ok(true);
            }
        }
        return Ok(false);
    }
    let Some((current, rest)) = expr_items.split_first() else {
        return Ok(false);
    };
    if !match_pattern(&pattern_items[0], current, literals, env, repeated)? {
        return Ok(false);
    }
    match_list_segments(
        &pattern_items[1..],
        pattern_tail,
        rest,
        expr_tail,
        literals,
        env,
        repeated,
    )
}

fn match_list_tail(
    pattern_tail: Option<&Expr>,
    expr_items: &[Expr],
    expr_tail: Option<&Expr>,
    literals: &HashSet<String>,
    env: &mut MatchEnv,
    repeated: bool,
) -> Result<bool, CompileError> {
    match pattern_tail {
        None => Ok(expr_items.is_empty() && expr_tail.is_none()),
        Some(pattern_tail) => {
            let remaining = rebuild_list_expr(expr_items, expr_tail);
            match_pattern(pattern_tail, &remaining, literals, env, repeated)
        }
    }
}

fn rebuild_list_expr(items: &[Expr], tail: Option<&Expr>) -> Expr {
    let span = items
        .iter()
        .fold(tail.map(|expr| expr.span).unwrap_or_default(), |span, expr| {
            span.merge(expr.span)
        });
    Expr {
        kind: ExprKind::List {
            items: items.to_vec(),
            tail: tail.cloned().map(Box::new),
        },
        span,
    }
}

fn repeat_count_for_template(template: &Expr, env: &MatchEnv) -> Result<usize, CompileError> {
    let mut count = None;
    collect_repeat_count(template, env, &mut count)?;
    count.ok_or_else(|| {
        CompileError::Expand(
            "ellipsis template must reference at least one repeated pattern variable".into(),
        )
    })
}

fn collect_repeat_count(
    template: &Expr,
    env: &MatchEnv,
    count: &mut Option<usize>,
) -> Result<(), CompileError> {
    match &template.kind {
        ExprKind::Symbol(symbol) => {
            if let Some(values) = env.repeats.get(symbol) {
                match count {
                    Some(existing) if *existing != values.len() => {
                        return Err(CompileError::Expand(
                            "mismatched ellipsis binding lengths in syntax-rules template".into(),
                        ));
                    }
                    Some(_) => {}
                    None => *count = Some(values.len()),
                }
            }
        }
        ExprKind::List { items, tail } => {
            for item in items {
                collect_repeat_count(item, env, count)?;
            }
            if let Some(tail) = tail {
                collect_repeat_count(tail, env, count)?;
            }
        }
        ExprKind::Quote(inner) => collect_repeat_count(inner, env, count)?,
        _ => {}
    }
    Ok(())
}

fn is_symbol(expr: &Expr, value: &str) -> bool {
    matches!(&expr.kind, ExprKind::Symbol(symbol) if symbol == value)
}

fn is_pattern_variable_name(symbol: &str, env: &MatchEnv) -> bool {
    env.singles.contains_key(symbol) || env.repeats.contains_key(symbol)
}

#[cfg(test)]
mod tests {
    use super::expand_program;
    use crate::frontend::ast::ExprKind;
    use crate::frontend::parse_program;
    use std::fs;
    use std::path::Path;

    #[test]
    fn expands_local_syntax_rules_macro() {
        let program = parse_program(
            "(define-syntax inc (syntax-rules () ((inc x) (+ x 1)))) (inc 4)\n",
        )
        .unwrap();
        let expanded =
            expand_program(Path::new("./tests/e2e/macro_test.scm"), &program)
                .unwrap();
        assert_eq!(expanded.forms.len(), 1);
        assert!(format!("{:?}", expanded.forms[0].kind).contains("List"));
    }

    #[test]
    fn expands_import_sets_and_library_renaming() {
        let root = Path::new("./build/expand-tests");
        fs::create_dir_all(root.join("lib")).unwrap();
        fs::write(
            root.join("lib/demo.sld"),
            "(define-library (lib demo)\n  (export (rename add2 sum2) inc)\n  (begin\n    (define (add2 x) (+ x 2))\n    (define-syntax inc (syntax-rules () ((inc x) (add2 x))))))\n",
        )
        .unwrap();

        let program = parse_program(
            "(import (prefix (rename (only (lib demo) sum2 inc) (sum2 add2)) demo:)) (demo:inc 4)\n",
        )
        .unwrap();
        let expanded = expand_program(&root.join("main.scm"), &program).unwrap();
        let rendered = format!("{:#?}", expanded.forms);
        assert!(rendered.contains("__lib_lib_demo_add2"));
        assert!(rendered.contains("demo:add2"));
        assert!(rendered.contains("+"));
    }

    #[test]
    fn expands_repeated_composite_patterns() {
        let program = parse_program(
            "(define-syntax pair-sums (syntax-rules () ((pair-sums ((a b) ...)) (list (+ a b) ...)))) (pair-sums ((1 2) (3 4)))\n",
        )
        .unwrap();
        let expanded = expand_program(Path::new("./tests/e2e/macro_test.scm"), &program).unwrap();
        let ExprKind::List { items, tail: None } = &expanded.forms[0].kind else {
            panic!("expected expanded pair-sums form to be a proper list");
        };
        assert!(matches!(&items[0].kind, ExprKind::Symbol(name) if name == "list"));
        assert_eq!(items.len(), 3);
        for (expected_lhs, expected_rhs, item) in [(1, 2, &items[1]), (3, 4, &items[2])] {
            let ExprKind::List { items: sum_items, tail: None } = &item.kind else {
                panic!("expected pair-sums element to be a proper list");
            };
            assert!(matches!(&sum_items[0].kind, ExprKind::Symbol(name) if name == "+"));
            assert!(matches!(&sum_items[1].kind, ExprKind::Integer(value) if *value == expected_lhs));
            assert!(matches!(&sum_items[2].kind, ExprKind::Integer(value) if *value == expected_rhs));
        }
    }

    #[test]
    fn expands_multiple_ellipsis_segments() {
        let program = parse_program(
            "(define-syntax gather (syntax-rules () ((gather (a ...) (b ...)) (list a ... b ...)))) (gather (1 2) (3 4))\n",
        )
        .unwrap();
        let expanded = expand_program(Path::new("./tests/e2e/macro_test.scm"), &program).unwrap();
        let ExprKind::List { items, tail: None } = &expanded.forms[0].kind else {
            panic!("expected expanded gather form to be a proper list");
        };
        assert!(matches!(&items[0].kind, ExprKind::Symbol(name) if name == "list"));
        assert_eq!(items.len(), 5);
        for (expected, item) in [1, 2, 3, 4].into_iter().zip(&items[1..]) {
            assert!(matches!(&item.kind, ExprKind::Integer(value) if *value == expected));
        }
    }

    #[test]
    fn hygienically_renames_introduced_binders() {
        let program = parse_program(
            "(define-syntax capture-test (syntax-rules () ((capture-test x) (let ((tmp 1)) x)))) (let ((tmp 9)) (capture-test tmp))\n",
        )
        .unwrap();
        let expanded = expand_program(Path::new("./tests/e2e/macro_test.scm"), &program).unwrap();
        let rendered = format!("{expanded:#?}");
        assert!(rendered.contains("__macro_tmp_"));
        assert!(!rendered.contains("kind: Symbol(\n                                \"tmp\""));
    }

    #[test]
    fn resolves_free_template_identifiers_at_definition_site() {
        let program = parse_program(
            "(define helper 1) (define-syntax use-helper (syntax-rules () ((use-helper) helper))) (let ((helper 9)) (use-helper))\n",
        )
        .unwrap();
        let expanded = expand_program(Path::new("./tests/e2e/macro_test.scm"), &program).unwrap();
        let rendered = format!("{expanded:#?}");
        assert!(rendered.contains("__macro_ref_helper_"));
        let last = expanded.forms.last().expect("expected expanded use");
        let ExprKind::List { items, .. } = &last.kind else {
            panic!("expected expanded use to be a list");
        };
        assert!(matches!(&items[0].kind, ExprKind::Symbol(name) if name == "let"));
        let body = items.last().expect("expected let body");
        assert!(matches!(&body.kind, ExprKind::Symbol(name) if name.starts_with("__macro_ref_helper_")));
    }
}
