use std::collections::HashMap;

use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::{BasicMetadataValueEnum, FunctionValue, IntValue, PointerValue};

use crate::backend::runtime::RuntimeAbi;
use crate::backend::statepoint::{attach_gc_strategy, gc_ptr_type};
use crate::error::CompileError;
use crate::middle::hir::Datum;
use crate::middle::hir::{Binding, Expr, ExprKind, Formals, Program, TopLevel};
use crate::runtime::layout::{BOOL_FALSE, BOOL_TRUE, EMPTY_LIST, FIXNUM_SHIFT, FIXNUM_TAG};
use crate::runtime::value::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompiledModule {
    pub module_name: String,
    pub llvm_ir: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HeapValueKind {
    Pair,
    String,
    Symbol,
    Vector,
    Box,
    Promise,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AbiValueKind {
    Word,
    Heap(HeapValueKind),
}

#[derive(Clone, Copy)]
struct FunctionInfo<'ctx> {
    value: FunctionValue<'ctx>,
    wrapper: FunctionValue<'ctx>,
    signature: FunctionSignature,
}

#[derive(Clone, Copy)]
struct ClosureInfo<'ctx> {
    ptr: PointerValue<'ctx>,
    signature: FunctionSignature,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct FunctionSignature {
    return_kind: AbiValueKind,
    required_param_kinds: &'static [AbiValueKind],
    rest: bool,
}

#[derive(Clone, Copy)]
struct ExceptionTarget<'ctx> {
    block: BasicBlock<'ctx>,
}

#[derive(Clone, Copy)]
enum BindingKind {
    Value(AbiValueKind),
    Function(FunctionSignature),
}

#[derive(Clone, Copy)]
enum CaptureKind {
    Value(AbiValueKind),
    Function(FunctionSignature),
}

#[derive(Clone, Copy)]
enum CodegenValue<'ctx> {
    Word(IntValue<'ctx>),
    RootedWord {
        slot: PointerValue<'ctx>,
    },
    HeapObject {
        ptr: PointerValue<'ctx>,
        kind: HeapValueKind,
    },
    MutableBox {
        ptr: PointerValue<'ctx>,
    },
    Function(FunctionInfo<'ctx>),
    Closure(ClosureInfo<'ctx>),
}

#[derive(Clone, Copy)]
enum RootedCallable<'ctx> {
    Function(FunctionInfo<'ctx>),
    Closure {
        slot: PointerValue<'ctx>,
        signature: FunctionSignature,
    },
}

pub struct LlvmBackend;

impl LlvmBackend {
    pub fn compile_program(
        module_name: &str,
        program: &Program,
    ) -> Result<CompiledModule, CompileError> {
        let context = Context::create();
        let mut compiler = Compiler::new(&context, module_name);
        compiler.compile_program(program)
    }
}

struct Compiler<'ctx> {
    context: &'ctx Context,
    module: Module<'ctx>,
    runtime: RuntimeAbi<'ctx>,
    functions: HashMap<String, FunctionInfo<'ctx>>,
    builtin_functions: HashMap<String, FunctionInfo<'ctx>>,
    mutable_top_level_names: std::collections::HashSet<String>,
    pair_mutated_top_level_names: std::collections::HashSet<String>,
    lambda_counter: usize,
    exception_targets: Vec<ExceptionTarget<'ctx>>,
}

impl<'ctx> Compiler<'ctx> {
    fn new(context: &'ctx Context, module_name: &str) -> Self {
        let module = context.create_module(module_name);
        let runtime = RuntimeAbi::declare(&module);
        Self {
            context,
            module,
            runtime,
            functions: HashMap::new(),
            builtin_functions: HashMap::new(),
            mutable_top_level_names: std::collections::HashSet::new(),
            pair_mutated_top_level_names: std::collections::HashSet::new(),
            lambda_counter: 0,
            exception_targets: Vec::new(),
        }
    }

    fn compile_program(&mut self, program: &Program) -> Result<CompiledModule, CompileError> {
        self.mutable_top_level_names = self.collect_program_mutations(program);
        self.pair_mutated_top_level_names = self.collect_program_pair_mutations(program);
        self.declare_builtin_procedures()?;
        self.declare_top_level_procedures(program)?;
        self.compile_builtin_procedures()?;
        self.compile_top_level_procedures(program)?;

        let builder = self.context.create_builder();
        let word = self.word_type();
        let main = self
            .module
            .add_function("main", word.fn_type(&[], false), None);
        attach_gc_strategy(main);
        let entry = self.context.append_basic_block(main, "entry");
        let exception_block = self.context.append_basic_block(main, "exception.return");
        builder.position_at_end(entry);
        self.exception_targets.push(ExceptionTarget {
            block: exception_block,
        });

        let mut env = HashMap::new();
        let mut last_value = CodegenValue::Word(self.const_fixnum(0));
        let mut rooted_top_level_count = 0usize;

        for item in &program.items {
            match item {
                TopLevel::Definition { name, value } => {
                    let compiled = self.compile_expr(&builder, main, &env, value)?;
                    let stored = if self.mutable_top_level_names.contains(name) {
                        self.box_value(&builder, compiled, &format!("top.level.{name}.box"))?
                    } else if self.pair_mutated_top_level_names.contains(name) {
                        rooted_top_level_count += 1;
                        self.root_word(
                            &builder,
                            self.value_to_word(
                                &builder,
                                compiled,
                                &format!("top.level.{name}.word"),
                            )?,
                            &format!("top.level.{name}.root"),
                        )?
                    } else {
                        compiled
                    };
                    env.insert(name.clone(), stored);
                    last_value = CodegenValue::Word(
                        self.word_type()
                            .const_int(Value::unspecified().bits() as u64, false),
                    );
                }
                TopLevel::Expression(expr) => {
                    last_value = self.compile_expr(&builder, main, &env, expr)?;
                }
                TopLevel::Procedure(_) => {}
            }
        }

        let return_value = self.value_to_word(&builder, last_value, "top.level.return")?;
        self.pop_root_slots(&builder, rooted_top_level_count)?;
        self.exception_targets.pop();
        builder
            .build_return(Some(&return_value))
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        self.emit_exception_return_block(
            exception_block,
            AbiValueKind::Word,
            rooted_top_level_count,
        )?;

        if main.verify(true) {
            Ok(CompiledModule {
                module_name: self
                    .module
                    .get_name()
                    .to_str()
                    .unwrap_or("module")
                    .to_string(),
                llvm_ir: self.module.print_to_string().to_string(),
            })
        } else {
            Err(CompileError::Codegen(
                "llvm verification failed for module".into(),
            ))
        }
    }

    fn declare_top_level_procedures(&mut self, program: &Program) -> Result<(), CompileError> {
        let function_signatures = self.infer_top_level_function_signatures(program);
        for item in &program.items {
            if let TopLevel::Procedure(procedure) = item {
                if self.functions.contains_key(&procedure.name) {
                    return Err(CompileError::Codegen(format!(
                        "duplicate procedure definition '{}'",
                        procedure.name
                    )));
                }

                let signature = function_signatures
                    .get(&procedure.name)
                    .copied()
                    .unwrap_or_else(|| self.default_signature(&procedure.formals));
                let wrapper_name = format!("__scheme_wrap_{}", sanitize_name(&procedure.name));
                let function =
                    self.module
                        .add_function(&procedure.name, self.function_type(signature), None);
                let wrapper =
                    self.module
                        .add_function(&wrapper_name, self.scheme_wrapper_type(), None);
                attach_gc_strategy(function);
                attach_gc_strategy(wrapper);
                self.functions.insert(
                    procedure.name.clone(),
                    FunctionInfo {
                        value: function,
                        wrapper,
                        signature,
                    },
                );
            }
        }
        Ok(())
    }

    fn compile_top_level_procedures(&mut self, program: &Program) -> Result<(), CompileError> {
        for item in &program.items {
            if let TopLevel::Procedure(procedure) = item {
                let function = *self.functions.get(&procedure.name).ok_or_else(|| {
                    CompileError::Codegen(format!("missing function '{}'", procedure.name))
                })?;
                self.compile_function_body(
                    function,
                    &procedure.formals,
                    &procedure.body,
                    &HashMap::new(),
                )?;
                self.compile_direct_scheme_wrapper(function, &procedure.formals)?;
            }
        }
        Ok(())
    }

    fn declare_builtin_procedures(&mut self) -> Result<(), CompileError> {
        for name in builtin_procedure_names() {
            let signature = builtin_wrapper_signature(name);
            let function_name = format!("__builtin_{}", sanitize_name(name));
            let wrapper_name = format!("__scheme_wrap_builtin_{}", sanitize_name(name));
            let function =
                self.module
                    .add_function(&function_name, self.function_type(signature), None);
            let wrapper = self
                .module
                .add_function(&wrapper_name, self.scheme_wrapper_type(), None);
            attach_gc_strategy(function);
            attach_gc_strategy(wrapper);
            self.builtin_functions.insert(
                (*name).to_string(),
                FunctionInfo {
                    value: function,
                    wrapper,
                    signature,
                },
            );
        }
        Ok(())
    }

    fn compile_builtin_procedures(&mut self) -> Result<(), CompileError> {
        for name in builtin_procedure_names() {
            let function = *self.builtin_functions.get(*name).ok_or_else(|| {
                CompileError::Codegen(format!("missing builtin wrapper for '{name}'"))
            })?;
            self.compile_builtin_wrapper(function, name)?;
            self.compile_builtin_scheme_wrapper(function, name)?;
        }
        Ok(())
    }

    fn compile_builtin_wrapper(
        &mut self,
        function: FunctionInfo<'ctx>,
        builtin_name: &str,
    ) -> Result<(), CompileError> {
        if function.value.get_first_basic_block().is_some() {
            return Ok(());
        }

        let builder = self.context.create_builder();
        let entry = self.context.append_basic_block(function.value, "entry");
        let exception_block = self
            .context
            .append_basic_block(function.value, "exception.return");
        builder.position_at_end(entry);
        self.exception_targets.push(ExceptionTarget {
            block: exception_block,
        });

        let mut env = HashMap::new();
        let mut prefix_args = Vec::with_capacity(
            function.signature.required_param_kinds.len() + usize::from(function.signature.rest),
        );
        for index in 0..function.signature.required_param_kinds.len() {
            let param_name = format!("arg{index}");
            let param = function.value.get_nth_param(index as u32).ok_or_else(|| {
                CompileError::Codegen(format!(
                    "missing builtin wrapper parameter {index} for '{builtin_name}'"
                ))
            })?;
            env.insert(
                param_name.clone(),
                CodegenValue::Word(param.into_int_value()),
            );
            prefix_args.push(Expr {
                kind: ExprKind::Variable(param_name),
            });
        }
        let tail_list = if function.signature.rest {
            let rest_name = "rest".to_string();
            let index = function.signature.required_param_kinds.len();
            let param = function.value.get_nth_param(index as u32).ok_or_else(|| {
                CompileError::Codegen(format!(
                    "missing builtin wrapper rest parameter for '{builtin_name}'"
                ))
            })?;
            env.insert(
                rest_name.clone(),
                CodegenValue::Word(param.into_int_value()),
            );
            param.into_int_value()
        } else {
            self.word_type().const_int(EMPTY_LIST as u64, false)
        };
        let argument_list = self.build_prefixed_list_value(
            &builder,
            function.value,
            &env,
            &prefix_args,
            tail_list,
            "builtin.args",
        )?;
        let result = builder
            .build_call(
                self.runtime.apply_builtin,
                &[
                    self.word_type()
                        .const_int(builtin_procedure_id(builtin_name) as u64, false)
                        .into(),
                    argument_list.into(),
                ],
                "builtin.apply",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("builtin apply did not return a value".into()))?
            .into_int_value();
        if function.signature.rest {
            debug_assert!(env.contains_key("rest"));
        }
        self.exception_targets.pop();
        builder
            .build_return(Some(&result))
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        self.emit_exception_return_block(exception_block, AbiValueKind::Word, 0)?;

        if function.value.verify(true) {
            Ok(())
        } else {
            Err(CompileError::Codegen(format!(
                "llvm verification failed for builtin wrapper '{}'",
                builtin_name
            )))
        }
    }

    fn compile_builtin_scheme_wrapper(
        &mut self,
        function: FunctionInfo<'ctx>,
        builtin_name: &str,
    ) -> Result<(), CompileError> {
        if function.wrapper.get_first_basic_block().is_some() {
            return Ok(());
        }

        let builder = self.context.create_builder();
        let entry = self.context.append_basic_block(function.wrapper, "entry");
        builder.position_at_end(entry);
        let args_list = function
            .wrapper
            .get_nth_param(1)
            .ok_or_else(|| {
                CompileError::Codegen(format!(
                    "missing Scheme wrapper args parameter for builtin '{builtin_name}'"
                ))
            })?
            .into_int_value();
        let result = builder
            .build_call(
                self.runtime.apply_builtin,
                &[
                    self.word_type()
                        .const_int(builtin_procedure_id(builtin_name) as u64, false)
                        .into(),
                    args_list.into(),
                ],
                "builtin.scheme.apply",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| {
                CompileError::Codegen("builtin Scheme wrapper did not return a value".into())
            })?
            .into_int_value();
        builder
            .build_return(Some(&result))
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        if function.wrapper.verify(true) {
            Ok(())
        } else {
            Err(CompileError::Codegen(format!(
                "llvm verification failed for builtin Scheme wrapper '{}'",
                builtin_name
            )))
        }
    }

    fn compile_direct_scheme_wrapper(
        &mut self,
        function: FunctionInfo<'ctx>,
        formals: &Formals,
    ) -> Result<(), CompileError> {
        if function.wrapper.get_first_basic_block().is_some() {
            return Ok(());
        }

        let builder = self.context.create_builder();
        let entry = self.context.append_basic_block(function.wrapper, "entry");
        builder.position_at_end(entry);
        let args_list = function
            .wrapper
            .get_nth_param(1)
            .ok_or_else(|| {
                CompileError::Codegen(format!(
                    "missing Scheme wrapper args parameter for function '{}'",
                    function.value.get_name().to_str().unwrap_or("<lambda>")
                ))
            })?
            .into_int_value();

        if !function.signature.rest {
            self.assert_list_length(
                &builder,
                function.wrapper,
                args_list,
                function.signature.required_param_kinds.len(),
            )?;
        } else {
            self.assert_list_length_at_least(
                &builder,
                function.wrapper,
                args_list,
                function.signature.required_param_kinds.len(),
            )?;
        }

        let mut args = Vec::with_capacity(
            function.signature.required_param_kinds.len() + usize::from(function.signature.rest),
        );
        for (index, kind) in function.signature.required_param_kinds.iter().enumerate() {
            let loaded =
                self.load_list_element(&builder, args_list, index, *kind, "scheme.wrapper.arg")?;
            args.push(self.convert_argument_value(
                &builder,
                loaded,
                *kind,
                "scheme.wrapper.arg",
            )?);
        }
        if formals.rest.is_some() {
            let rest = self.list_tail_word(
                &builder,
                args_list,
                function.signature.required_param_kinds.len(),
                "scheme.wrapper.rest",
            )?;
            args.push(rest.into());
        }

        let result = builder
            .build_call(function.value, &args, "scheme.wrapper.call")
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("Scheme wrapper did not return a value".into()))?;
        let tail_pending = builder
            .build_call(
                self.runtime.rt_tail_pending,
                &[],
                "scheme.wrapper.tail_pending",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("tail pending did not return a value".into()))?
            .into_int_value();
        let tail_block = self
            .context
            .append_basic_block(function.wrapper, "tail.pending");
        let value_block = self
            .context
            .append_basic_block(function.wrapper, "tail.value");
        builder
            .build_conditional_branch(tail_pending, tail_block, value_block)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;

        builder.position_at_end(tail_block);
        let marker = builder
            .build_call(
                self.runtime.rt_tail_call_marker,
                &[],
                "scheme.wrapper.tail_marker",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("tail marker did not return a value".into()))?
            .into_int_value();
        builder
            .build_return(Some(&marker))
            .map_err(|error| CompileError::Codegen(error.to_string()))?;

        builder.position_at_end(value_block);
        let wrapped = match function.signature.return_kind {
            AbiValueKind::Word => result.into_int_value(),
            AbiValueKind::Heap(_) => builder
                .build_ptr_to_int(
                    result.into_pointer_value(),
                    self.word_type(),
                    "scheme.wrapper.word",
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?,
        };
        builder
            .build_return(Some(&wrapped))
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        if function.wrapper.verify(true) {
            Ok(())
        } else {
            Err(CompileError::Codegen(format!(
                "llvm verification failed for Scheme wrapper '{}'",
                function.wrapper.get_name().to_str().unwrap_or("<wrapper>")
            )))
        }
    }

    fn compile_function_body(
        &mut self,
        function: FunctionInfo<'ctx>,
        formals: &Formals,
        body: &Expr,
        outer_env: &HashMap<String, CodegenValue<'ctx>>,
    ) -> Result<(), CompileError> {
        if function.value.get_first_basic_block().is_some() {
            return Ok(());
        }

        let builder = self.context.create_builder();
        let entry = self.context.append_basic_block(function.value, "entry");
        let exception_block = self
            .context
            .append_basic_block(function.value, "exception.return");
        builder.position_at_end(entry);
        self.exception_targets.push(ExceptionTarget {
            block: exception_block,
        });

        let mut env = outer_env.clone();
        let formal_names = formals.all_names();
        let mutated_names = self.collect_mutated_names_with_initial(body, formal_names.clone());
        let pair_mutated_names = self.collect_pair_mutated_names_with_initial(body, formal_names);
        let mut rooted_param_count = 0usize;
        for (index, param_name) in formals.required.iter().enumerate() {
            let param = function.value.get_nth_param(index as u32).ok_or_else(|| {
                CompileError::Codegen(format!(
                    "missing parameter {index} for function '{}'",
                    function.value.get_name().to_str().unwrap_or("<lambda>")
                ))
            })?;
            let value = match function
                .signature
                .required_param_kinds
                .get(index)
                .copied()
                .unwrap_or(AbiValueKind::Word)
            {
                AbiValueKind::Word => CodegenValue::Word(param.into_int_value()),
                AbiValueKind::Heap(kind) => CodegenValue::HeapObject {
                    ptr: param.into_pointer_value(),
                    kind,
                },
            };
            let stored = if mutated_names.contains(param_name) {
                self.box_value(&builder, value, &format!("param.{param_name}.box"))?
            } else if pair_mutated_names.contains(param_name) {
                rooted_param_count += 1;
                self.root_word(
                    &builder,
                    self.value_to_word(&builder, value, &format!("param.{param_name}.word"))?,
                    &format!("param.{param_name}.root"),
                )?
            } else {
                value
            };
            env.insert(param_name.clone(), stored);
        }
        if let Some(rest_name) = &formals.rest {
            let index = formals.required.len();
            let param = function.value.get_nth_param(index as u32).ok_or_else(|| {
                CompileError::Codegen(format!(
                    "missing rest parameter for function '{}'",
                    function.value.get_name().to_str().unwrap_or("<lambda>")
                ))
            })?;
            let value = CodegenValue::Word(param.into_int_value());
            let stored = if mutated_names.contains(rest_name) {
                self.box_value(&builder, value, &format!("param.{rest_name}.box"))?
            } else if pair_mutated_names.contains(rest_name) {
                rooted_param_count += 1;
                self.root_word(
                    &builder,
                    self.value_to_word(&builder, value, &format!("param.{rest_name}.word"))?,
                    &format!("param.{rest_name}.root"),
                )?
            } else {
                value
            };
            env.insert(rest_name.clone(), stored);
        }

        self.compile_tail_expr(
            &builder,
            function.value,
            function.signature,
            &env,
            body,
            rooted_param_count,
        )?;
        self.emit_exception_return_block(
            exception_block,
            function.signature.return_kind,
            rooted_param_count,
        )?;
        self.exception_targets.pop();

        if function.value.verify(true) {
            Ok(())
        } else {
            Err(CompileError::Codegen(format!(
                "llvm verification failed for function '{}'",
                function.value.get_name().to_str().unwrap_or("<lambda>")
            )))
        }
    }

    fn compile_expr(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        expr: &Expr,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        match &expr.kind {
            ExprKind::Unspecified => Ok(CodegenValue::Word(
                self.word_type()
                    .const_int(Value::unspecified().bits() as u64, false),
            )),
            ExprKind::Integer(value) => Ok(CodegenValue::Word(self.const_fixnum_checked(*value)?)),
            ExprKind::Boolean(value) => Ok(CodegenValue::Word(self.const_bool(*value))),
            ExprKind::Char(value) => Ok(CodegenValue::Word(
                self.word_type()
                    .const_int(Value::encode_char(*value).bits() as u64, false),
            )),
            ExprKind::String(value) => self.compile_string_literal(builder, value),
            ExprKind::Variable(name) => match env.get(name).copied() {
                Some(CodegenValue::MutableBox { ptr }) => Ok(CodegenValue::Word(
                    self.load_box_word(builder, ptr, &format!("{name}.load"))?,
                )),
                Some(CodegenValue::RootedWord { slot }) => Ok(CodegenValue::Word(
                    self.load_rooted_word(builder, slot, &format!("{name}.root.load"))?,
                )),
                Some(value) => Ok(value),
                None => self
                    .functions
                    .get(name)
                    .copied()
                    .map(CodegenValue::Function)
                    .or_else(|| {
                        self.builtin_functions
                            .get(name)
                            .copied()
                            .map(CodegenValue::Function)
                    })
                    .ok_or_else(|| CompileError::Codegen(format!("undefined variable '{name}'"))),
            },
            ExprKind::Set { name, value } => {
                self.compile_set(builder, current_function, env, name, value)
            }
            ExprKind::Begin(exprs) => {
                let mut last = CodegenValue::Word(self.const_fixnum(0));
                for expr in exprs {
                    last = self.compile_expr(builder, current_function, env, expr)?;
                }
                Ok(last)
            }
            ExprKind::Let { bindings, body } => {
                let (scoped, rooted_count) =
                    self.compile_parallel_bindings(builder, current_function, env, bindings, body)?;
                let result = self.compile_expr(builder, current_function, &scoped, body)?;
                self.pop_root_slots(builder, rooted_count)?;
                Ok(result)
            }
            ExprKind::LetStar { bindings, body } => {
                let (scoped, rooted_count) = self.compile_sequential_bindings(
                    builder,
                    current_function,
                    env,
                    bindings,
                    body,
                )?;
                let result = self.compile_expr(builder, current_function, &scoped, body)?;
                self.pop_root_slots(builder, rooted_count)?;
                Ok(result)
            }
            ExprKind::LetRec { bindings, body } => {
                let scoped =
                    self.compile_recursive_bindings(builder, current_function, env, bindings)?;
                let mut merged = env.clone();
                merged.extend(scoped);
                self.compile_expr(builder, current_function, &merged, body)
            }
            ExprKind::Guard {
                name,
                handler,
                body,
            } => self.compile_guard(builder, current_function, env, name, handler, body),
            ExprKind::Delay(expr) => self.compile_delay(builder, env, expr),
            ExprKind::Force(expr) => self.compile_force(builder, current_function, env, expr),
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let condition_result =
                    self.compile_expr(builder, current_function, env, condition)?;
                let predicate = match condition_result {
                    CodegenValue::HeapObject { .. } | CodegenValue::Closure(_) => {
                        self.context.bool_type().const_int(1, false)
                    }
                    other => {
                        let condition_value = self.expect_word(other, "if condition")?;
                        builder
                            .build_int_compare(
                                IntPredicate::NE,
                                condition_value,
                                self.const_bool(false),
                                "if.truthy",
                            )
                            .map_err(|error| CompileError::Codegen(error.to_string()))?
                    }
                };

                let then_block = self.context.append_basic_block(current_function, "if.then");
                let else_block = self.context.append_basic_block(current_function, "if.else");
                let merge_block = self
                    .context
                    .append_basic_block(current_function, "if.merge");

                builder
                    .build_conditional_branch(predicate, then_block, else_block)
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;

                builder.position_at_end(then_block);
                let then_result = self.compile_expr(builder, current_function, env, then_branch)?;
                builder
                    .build_unconditional_branch(merge_block)
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
                let then_block = builder
                    .get_insert_block()
                    .ok_or_else(|| CompileError::Codegen("missing then block".into()))?;

                builder.position_at_end(else_block);
                let else_result = self.compile_expr(builder, current_function, env, else_branch)?;
                builder
                    .build_unconditional_branch(merge_block)
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
                let else_block = builder
                    .get_insert_block()
                    .ok_or_else(|| CompileError::Codegen("missing else block".into()))?;

                builder.position_at_end(merge_block);
                self.merge_branch_values(
                    builder,
                    then_result,
                    then_block,
                    else_result,
                    else_block,
                    "if.result",
                )
            }
            ExprKind::Call { callee, args } => {
                let result = self.compile_call(builder, current_function, env, callee, args)?;
                self.branch_on_pending_exception(builder, current_function, result)
            }
            ExprKind::Lambda { formals, body } => self.compile_lambda(builder, env, formals, body),
            ExprKind::Quote(datum) => self.compile_quote(builder, datum),
        }
    }

    fn compile_tail_expr(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        current_signature: FunctionSignature,
        env: &HashMap<String, CodegenValue<'ctx>>,
        expr: &Expr,
        cleanup_roots: usize,
    ) -> Result<(), CompileError> {
        match &expr.kind {
            ExprKind::Begin(exprs) => {
                if exprs.is_empty() {
                    return self.tail_return_value(
                        builder,
                        cleanup_roots,
                        current_signature.return_kind,
                        CodegenValue::Word(
                            self.word_type()
                                .const_int(Value::unspecified().bits() as u64, false),
                        ),
                    );
                }
                for expr in &exprs[..exprs.len() - 1] {
                    let _ = self.compile_expr(builder, current_function, env, expr)?;
                }
                self.compile_tail_expr(
                    builder,
                    current_function,
                    current_signature,
                    env,
                    &exprs[exprs.len() - 1],
                    cleanup_roots,
                )
            }
            ExprKind::Let { bindings, body } => {
                let (scoped, rooted_count) =
                    self.compile_parallel_bindings(builder, current_function, env, bindings, body)?;
                self.compile_tail_expr(
                    builder,
                    current_function,
                    current_signature,
                    &scoped,
                    body,
                    cleanup_roots + rooted_count,
                )
            }
            ExprKind::LetStar { bindings, body } => {
                let (scoped, rooted_count) = self.compile_sequential_bindings(
                    builder,
                    current_function,
                    env,
                    bindings,
                    body,
                )?;
                self.compile_tail_expr(
                    builder,
                    current_function,
                    current_signature,
                    &scoped,
                    body,
                    cleanup_roots + rooted_count,
                )
            }
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let condition_result =
                    self.compile_expr(builder, current_function, env, condition)?;
                let predicate = match condition_result {
                    CodegenValue::HeapObject { .. } | CodegenValue::Closure(_) => {
                        self.context.bool_type().const_int(1, false)
                    }
                    other => {
                        let condition_value = self.expect_word(other, "if condition")?;
                        builder
                            .build_int_compare(
                                IntPredicate::NE,
                                condition_value,
                                self.const_bool(false),
                                "if.truthy",
                            )
                            .map_err(|error| CompileError::Codegen(error.to_string()))?
                    }
                };

                let then_block = self.context.append_basic_block(current_function, "if.then");
                let else_block = self.context.append_basic_block(current_function, "if.else");
                builder
                    .build_conditional_branch(predicate, then_block, else_block)
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;

                builder.position_at_end(then_block);
                self.compile_tail_expr(
                    builder,
                    current_function,
                    current_signature,
                    env,
                    then_branch,
                    cleanup_roots,
                )?;

                builder.position_at_end(else_block);
                self.compile_tail_expr(
                    builder,
                    current_function,
                    current_signature,
                    env,
                    else_branch,
                    cleanup_roots,
                )
            }
            ExprKind::Guard {
                name,
                handler,
                body,
            } => {
                let body_block = self
                    .context
                    .append_basic_block(current_function, "guard.body");
                let handler_block = self
                    .context
                    .append_basic_block(current_function, "guard.handler");
                builder
                    .build_unconditional_branch(body_block)
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;

                builder.position_at_end(body_block);
                self.exception_targets.push(ExceptionTarget {
                    block: handler_block,
                });
                self.compile_tail_expr(
                    builder,
                    current_function,
                    current_signature,
                    env,
                    body,
                    cleanup_roots,
                )?;
                self.exception_targets.pop();

                builder.position_at_end(handler_block);
                let exception = builder
                    .build_call(
                        self.runtime.rt_take_pending_exception,
                        &[],
                        "guard.exception",
                    )
                    .map_err(|error| CompileError::Codegen(error.to_string()))?
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| {
                        CompileError::Codegen(
                            "take-pending-exception did not return a value".into(),
                        )
                    })?
                    .into_int_value();
                let mut guarded_env = env.clone();
                guarded_env.insert(name.to_string(), CodegenValue::Word(exception));
                self.compile_tail_expr(
                    builder,
                    current_function,
                    current_signature,
                    &guarded_env,
                    handler,
                    cleanup_roots,
                )
            }
            ExprKind::Call { callee, args } => self.compile_tail_call(
                builder,
                current_function,
                current_signature,
                env,
                callee,
                args,
                cleanup_roots,
            ),
            _ => {
                let value = self.compile_expr(builder, current_function, env, expr)?;
                self.tail_return_value(builder, cleanup_roots, current_signature.return_kind, value)
            }
        }
    }

    fn compile_call(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        callee: &Expr,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if let ExprKind::Variable(name) = &callee.kind {
            if is_builtin(name) && !env.contains_key(name) {
                return self.compile_builtin_call(builder, current_function, env, name, args);
            }
        }

        let callee_value = self.compile_expr(builder, current_function, env, callee)?;
        let (callee_value, signature) =
            self.resolve_callable_value(builder, callee_value, args.len())?;
        if args.len() < signature.required_param_kinds.len()
            || (!signature.rest && args.len() != signature.required_param_kinds.len())
        {
            return Err(CompileError::Codegen(format!(
                "procedure expects {}{} arguments but got {}",
                signature.required_param_kinds.len(),
                if signature.rest { " or more" } else { "" },
                args.len()
            )));
        }
        let mut compiled_args = args
            .iter()
            .zip(signature.required_param_kinds.iter())
            .map(|(expr, kind)| {
                let value = self.compile_expr(builder, current_function, env, expr)?;
                self.convert_argument_value(builder, value, *kind, "procedure.argument")
            })
            .collect::<Result<Vec<_>, _>>()?;
        if signature.rest {
            let rest_list = self.build_list_value(
                builder,
                current_function,
                env,
                &args[signature.required_param_kinds.len()..],
                "call.rest",
            )?;
            compiled_args.push(rest_list.into());
        }
        self.emit_callable_call(builder, callee_value, signature, compiled_args)
    }

    fn compile_tail_call(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        current_signature: FunctionSignature,
        env: &HashMap<String, CodegenValue<'ctx>>,
        callee: &Expr,
        args: &[Expr],
        cleanup_roots: usize,
    ) -> Result<(), CompileError> {
        if let ExprKind::Variable(name) = &callee.kind {
            if is_builtin(name) && !env.contains_key(name) {
                let result =
                    self.compile_builtin_call(builder, current_function, env, name, args)?;
                return self.tail_return_value(
                    builder,
                    cleanup_roots,
                    current_signature.return_kind,
                    result,
                );
            }
        }

        let callee_value = self.compile_expr(builder, current_function, env, callee)?;
        let (callee_value, signature) =
            self.resolve_callable_value(builder, callee_value, args.len())?;
        if args.len() < signature.required_param_kinds.len()
            || (!signature.rest && args.len() != signature.required_param_kinds.len())
        {
            return Err(CompileError::Codegen(format!(
                "procedure expects {}{} arguments but got {}",
                signature.required_param_kinds.len(),
                if signature.rest { " or more" } else { "" },
                args.len()
            )));
        }
        let mut compiled_args = args
            .iter()
            .zip(signature.required_param_kinds.iter())
            .map(|(expr, kind)| {
                let value = self.compile_expr(builder, current_function, env, expr)?;
                self.convert_argument_value(builder, value, *kind, "procedure.argument")
            })
            .collect::<Result<Vec<_>, _>>()?;
        if signature.rest {
            let rest_list = self.build_list_value(
                builder,
                current_function,
                env,
                &args[signature.required_param_kinds.len()..],
                "call.rest",
            )?;
            compiled_args.push(rest_list.into());
        }
        let args_list =
            self.build_scheme_args_list(builder, signature, &compiled_args, "tail.call.args")?;
        self.emit_tail_request(
            builder,
            current_signature.return_kind,
            callee_value,
            signature,
            args_list,
            cleanup_roots,
        )
    }

    fn compile_builtin_call(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        callee: &str,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        match callee {
            "+" | "-" | "*" | "/" => {
                self.compile_numeric_builtin(builder, current_function, env, callee, args)
            }
            "=" | "<" | ">" | "<=" | ">=" => {
                self.compile_numeric_comparison(builder, current_function, env, callee, args)
            }
            "not" => self.compile_not(builder, current_function, env, args),
            "boolean?" => self.compile_boolean_predicate(builder, current_function, env, args),
            "zero?" => self.compile_zero_predicate(builder, current_function, env, args),
            "char?" => self.compile_char_predicate(builder, current_function, env, args),
            "char=?" | "char<?" | "char>?" | "char<=?" | "char>=?" => {
                self.compile_char_comparison(builder, current_function, env, callee, args)
            }
            "char->integer" => self.compile_char_to_integer(builder, current_function, env, args),
            "integer->char" => self.compile_integer_to_char(builder, current_function, env, args),
            "symbol?" => self.compile_symbol_predicate(builder, current_function, env, args),
            "symbol->string" => self.compile_symbol_to_string(builder, current_function, env, args),
            "string->symbol" => self.compile_string_to_symbol(builder, current_function, env, args),
            "procedure?" => self.compile_procedure_predicate(builder, current_function, env, args),
            "values" => self.compile_values(builder, current_function, env, args),
            "call-with-values" => {
                self.compile_call_with_values(builder, current_function, env, args)
            }
            "raise" => self.compile_raise(builder, current_function, env, args),
            "error" => self.compile_error(builder, current_function, env, args),
            "apply" => self.compile_apply(builder, current_function, env, args),
            "eq?" | "eqv?" => {
                self.compile_identity_predicate(builder, current_function, env, callee, args)
            }
            "equal?" => self.compile_equal_predicate(builder, current_function, env, args),
            "list" => self.compile_list(builder, current_function, env, args),
            "map" => self.compile_map(builder, current_function, env, args),
            "for-each" => self.compile_for_each(builder, current_function, env, args),
            "append" => self.compile_append(builder, current_function, env, args),
            "memq" => self.compile_member_like(
                builder,
                current_function,
                env,
                args,
                self.runtime.memq,
                "memq",
            ),
            "memv" => self.compile_member_like(
                builder,
                current_function,
                env,
                args,
                self.runtime.memv,
                "memv",
            ),
            "member" => self.compile_member_like(
                builder,
                current_function,
                env,
                args,
                self.runtime.member,
                "member",
            ),
            "assq" => self.compile_member_like(
                builder,
                current_function,
                env,
                args,
                self.runtime.assq,
                "assq",
            ),
            "assv" => self.compile_member_like(
                builder,
                current_function,
                env,
                args,
                self.runtime.assv,
                "assv",
            ),
            "assoc" => self.compile_member_like(
                builder,
                current_function,
                env,
                args,
                self.runtime.assoc,
                "assoc",
            ),
            "list-copy" => self.compile_list_copy(builder, current_function, env, args),
            "reverse" => self.compile_reverse(builder, current_function, env, args),
            "cons" => self.compile_cons(builder, current_function, env, args),
            "car" => self.compile_pair_access(builder, current_function, env, args, true),
            "cdr" => self.compile_pair_access(builder, current_function, env, args, false),
            "set-car!" => self.compile_pair_set(builder, current_function, env, args, true),
            "set-cdr!" => self.compile_pair_set(builder, current_function, env, args, false),
            "pair?" => self.compile_pair_predicate(builder, current_function, env, args),
            "list?" => self.compile_list_predicate(builder, current_function, env, args),
            "length" => self.compile_list_length(builder, current_function, env, args),
            "list-tail" => self.compile_list_tail(builder, current_function, env, args),
            "list-ref" => self.compile_list_ref(builder, current_function, env, args),
            "null?" => self.compile_null_predicate(builder, current_function, env, args),
            "string?" => self.compile_string_predicate(builder, current_function, env, args),
            "string-length" => self.compile_string_length(builder, current_function, env, args),
            "string-ref" => self.compile_string_ref(builder, current_function, env, args),
            "display" => self.compile_display(builder, current_function, env, args),
            "write" => self.compile_write(builder, current_function, env, args),
            "newline" => self.compile_newline(builder, current_function, env, args),
            "gc-stress" => self.compile_gc_stress(builder, current_function, env, args),
            "vector" => self.compile_vector(builder, current_function, env, args),
            "vector?" => self.compile_vector_predicate(builder, current_function, env, args),
            "vector-length" => self.compile_vector_length(builder, current_function, env, args),
            "vector-ref" => self.compile_vector_ref(builder, current_function, env, args),
            "vector-set!" => self.compile_vector_set(builder, current_function, env, args),
            _ => unreachable!(),
        }
    }

    fn compile_delay(
        &mut self,
        builder: &Builder<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        expr: &Expr,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let thunk = self.compile_lambda(
            builder,
            env,
            &Formals {
                required: Vec::new(),
                rest: None,
            },
            expr,
        )?;
        let thunk = match thunk {
            CodegenValue::Function(info) => {
                self.allocate_placeholder_closure(builder, info.wrapper, info.signature, 0)?
            }
            other => other,
        };
        let thunk_word = self.value_to_word(builder, thunk, "delay.thunk.word")?;
        let promise = builder
            .build_call(
                self.runtime.alloc_promise_gc,
                &[thunk_word.into()],
                "promise.alloc",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| {
                CompileError::Codegen("promise allocation did not return a value".into())
            })?
            .into_pointer_value();
        Ok(CodegenValue::HeapObject {
            ptr: promise,
            kind: HeapValueKind::Promise,
        })
    }

    fn compile_force(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        expr: &Expr,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let promise = self.compile_expr(builder, current_function, env, expr)?;
        match promise {
            CodegenValue::HeapObject {
                ptr,
                kind: HeapValueKind::Promise,
            } => self.compile_force_promise_ptr(builder, current_function, ptr),
            other => {
                let promise_word = self.expect_word(other, "force argument")?;
                self.compile_force_promise_word(builder, current_function, promise_word)
            }
        }
    }

    fn compile_force_promise_ptr(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        promise_ptr: PointerValue<'ctx>,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let forced = builder
            .build_call(
                self.runtime.promise_forced_gc,
                &[promise_ptr.into()],
                "promise.forced",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("promise-forced did not return a value".into()))?
            .into_int_value();
        let forced_block = self
            .context
            .append_basic_block(current_function, "promise.forced");
        let evaluate_block = self
            .context
            .append_basic_block(current_function, "promise.eval");
        let merge_block = self
            .context
            .append_basic_block(current_function, "promise.merge");
        builder
            .build_conditional_branch(forced, forced_block, evaluate_block)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;

        builder.position_at_end(forced_block);
        let forced_value = builder
            .build_call(
                self.runtime.promise_value_gc,
                &[promise_ptr.into()],
                "promise.value",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("promise-value did not return a value".into()))?
            .into_int_value();
        builder
            .build_unconditional_branch(merge_block)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let forced_block = builder
            .get_insert_block()
            .ok_or_else(|| CompileError::Codegen("missing promise forced block".into()))?;

        builder.position_at_end(evaluate_block);
        let thunk_word = builder
            .build_call(
                self.runtime.promise_value_gc,
                &[promise_ptr.into()],
                "promise.thunk",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("promise thunk did not return a value".into()))?
            .into_int_value();
        let thunk_signature = FunctionSignature {
            return_kind: AbiValueKind::Word,
            required_param_kinds: &[],
            rest: false,
        };
        let thunk_ptr = builder
            .build_int_to_ptr(thunk_word, gc_ptr_type(self.context), "promise.thunk.ptr")
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let result = self.emit_callable_call(
            builder,
            CodegenValue::Closure(ClosureInfo {
                ptr: thunk_ptr,
                signature: thunk_signature,
            }),
            thunk_signature,
            Vec::new(),
        )?;
        let result = self.branch_on_pending_exception(builder, current_function, result)?;
        let result_word = self.expect_word(result, "promise force result")?;
        let resolved_word = builder
            .build_call(
                self.runtime.promise_resolve_gc,
                &[promise_ptr.into(), result_word.into()],
                "promise.resolve",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("promise-resolve did not return a value".into()))?
            .into_int_value();
        builder
            .build_unconditional_branch(merge_block)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let evaluate_block = builder
            .get_insert_block()
            .ok_or_else(|| CompileError::Codegen("missing promise eval block".into()))?;

        builder.position_at_end(merge_block);
        let phi = builder
            .build_phi(self.context.i64_type(), "promise.result")
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        phi.add_incoming(&[
            (&forced_value, forced_block),
            (&resolved_word, evaluate_block),
        ]);
        Ok(CodegenValue::Word(phi.as_basic_value().into_int_value()))
    }

    fn compile_force_promise_word(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        promise_word: IntValue<'ctx>,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let forced = builder
            .build_call(
                self.runtime.promise_forced,
                &[promise_word.into()],
                "promise.forced.word",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("promise-forced did not return a value".into()))?
            .into_int_value();
        let forced_block = self
            .context
            .append_basic_block(current_function, "promise.word.forced");
        let evaluate_block = self
            .context
            .append_basic_block(current_function, "promise.word.eval");
        let merge_block = self
            .context
            .append_basic_block(current_function, "promise.word.merge");
        builder
            .build_conditional_branch(forced, forced_block, evaluate_block)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;

        builder.position_at_end(forced_block);
        let forced_value = builder
            .build_call(
                self.runtime.promise_value,
                &[promise_word.into()],
                "promise.value.word",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("promise-value did not return a value".into()))?
            .into_int_value();
        builder
            .build_unconditional_branch(merge_block)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let forced_block = builder
            .get_insert_block()
            .ok_or_else(|| CompileError::Codegen("missing promise word forced block".into()))?;

        builder.position_at_end(evaluate_block);
        let thunk_word = builder
            .build_call(
                self.runtime.promise_value,
                &[promise_word.into()],
                "promise.thunk.word",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("promise thunk did not return a value".into()))?
            .into_int_value();
        let thunk_signature = FunctionSignature {
            return_kind: AbiValueKind::Word,
            required_param_kinds: &[],
            rest: false,
        };
        let thunk_ptr = builder
            .build_int_to_ptr(
                thunk_word,
                gc_ptr_type(self.context),
                "promise.word.thunk.ptr",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let result = self.emit_callable_call(
            builder,
            CodegenValue::Closure(ClosureInfo {
                ptr: thunk_ptr,
                signature: thunk_signature,
            }),
            thunk_signature,
            Vec::new(),
        )?;
        let result = self.branch_on_pending_exception(builder, current_function, result)?;
        let result_word = self.expect_word(result, "promise force result")?;
        let resolved_word = builder
            .build_call(
                self.runtime.promise_resolve,
                &[promise_word.into(), result_word.into()],
                "promise.resolve.word",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("promise-resolve did not return a value".into()))?
            .into_int_value();
        builder
            .build_unconditional_branch(merge_block)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let evaluate_block = builder
            .get_insert_block()
            .ok_or_else(|| CompileError::Codegen("missing promise word eval block".into()))?;

        builder.position_at_end(merge_block);
        let phi = builder
            .build_phi(self.context.i64_type(), "promise.word.result")
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        phi.add_incoming(&[
            (&forced_value, forced_block),
            (&resolved_word, evaluate_block),
        ]);
        Ok(CodegenValue::Word(phi.as_basic_value().into_int_value()))
    }

    fn compile_string_literal(
        &mut self,
        builder: &Builder<'ctx>,
        value: &str,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let name = format!("__string_literal_{}", self.lambda_counter);
        self.lambda_counter += 1;
        let bytes = value.as_bytes();
        let const_bytes = self.context.const_string(bytes, false);
        let global = self.module.add_global(const_bytes.get_type(), None, &name);
        global.set_initializer(&const_bytes);
        global.set_constant(true);
        let pointer = builder
            .build_pointer_cast(
                global.as_pointer_value(),
                self.context.ptr_type(inkwell::AddressSpace::default()),
                &format!("{name}.ptr"),
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let length = self.word_type().const_int(bytes.len() as u64, false);
        let call = builder
            .build_call(
                self.runtime.alloc_string_gc,
                &[pointer.into(), length.into()],
                &name,
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let result = call.try_as_basic_value().basic().ok_or_else(|| {
            CompileError::Codegen("string literal allocation did not return a value".into())
        })?;
        Ok(CodegenValue::HeapObject {
            ptr: result.into_pointer_value(),
            kind: HeapValueKind::String,
        })
    }

    fn compile_symbol_literal(
        &mut self,
        builder: &Builder<'ctx>,
        value: &str,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let name = format!("__symbol_literal_{}", self.lambda_counter);
        self.lambda_counter += 1;
        let bytes = value.as_bytes();
        let const_bytes = self.context.const_string(bytes, false);
        let global = self.module.add_global(const_bytes.get_type(), None, &name);
        global.set_initializer(&const_bytes);
        global.set_constant(true);
        let pointer = builder
            .build_pointer_cast(
                global.as_pointer_value(),
                self.context.ptr_type(inkwell::AddressSpace::default()),
                &format!("{name}.ptr"),
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let length = self.word_type().const_int(bytes.len() as u64, false);
        let call = builder
            .build_call(
                self.runtime.alloc_symbol_gc,
                &[pointer.into(), length.into()],
                &name,
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let result = call.try_as_basic_value().basic().ok_or_else(|| {
            CompileError::Codegen("symbol literal allocation did not return a value".into())
        })?;
        Ok(CodegenValue::HeapObject {
            ptr: result.into_pointer_value(),
            kind: HeapValueKind::Symbol,
        })
    }

    fn compile_numeric_builtin(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        callee: &str,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let untagged_args = args
            .iter()
            .enumerate()
            .map(|(index, expr)| {
                let value = self.compile_expr(builder, current_function, env, expr)?;
                let word = self.expect_word(value, "builtin argument")?;
                let checked = self.ensure_fixnum(
                    builder,
                    current_function,
                    word,
                    &format!("arg{index}.fixnum.check"),
                )?;
                self.decode_fixnum(builder, checked, &format!("arg{index}.fixnum"))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let i64_type = self.word_type();
        let zero = i64_type.const_zero();
        let raw = match callee {
            "+" => untagged_args.into_iter().try_fold(zero, |acc, value| {
                builder
                    .build_int_add(acc, value, "addtmp")
                    .map_err(|error| CompileError::Codegen(error.to_string()))
            })?,
            "*" => {
                untagged_args
                    .into_iter()
                    .try_fold(i64_type.const_int(1, false), |acc, value| {
                        builder
                            .build_int_mul(acc, value, "multmp")
                            .map_err(|error| CompileError::Codegen(error.to_string()))
                    })?
            }
            "-" => match untagged_args.as_slice() {
                [] => {
                    return Err(CompileError::Codegen(
                        "operator '-' expects at least one argument".into(),
                    ));
                }
                [value] => builder
                    .build_int_sub(zero, *value, "negtmp")
                    .map_err(|error| CompileError::Codegen(error.to_string()))?,
                [first, rest @ ..] => rest.iter().copied().try_fold(*first, |acc, value| {
                    builder
                        .build_int_sub(acc, value, "subtmp")
                        .map_err(|error| CompileError::Codegen(error.to_string()))
                })?,
            },
            "/" => match untagged_args.as_slice() {
                [] | [_] => {
                    return Err(CompileError::Codegen(
                        "operator '/' expects at least two arguments".into(),
                    ));
                }
                [first, rest @ ..] => rest.iter().copied().try_fold(*first, |acc, value| {
                    builder
                        .build_int_signed_div(acc, value, "divtmp")
                        .map_err(|error| CompileError::Codegen(error.to_string()))
                })?,
            },
            _ => unreachable!(),
        };

        Ok(CodegenValue::Word(self.encode_fixnum_value(
            builder,
            raw,
            "tagged.result",
        )?))
    }

    fn compile_not(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "not expects exactly one argument".into(),
            ));
        }
        let value = self.compile_expr(builder, current_function, env, &args[0])?;
        let predicate = match value {
            CodegenValue::HeapObject { .. } | CodegenValue::Closure(_) => {
                self.context.bool_type().const_zero()
            }
            other => {
                let word = self.expect_word(other, "not argument")?;
                builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        word,
                        self.const_bool(false),
                        "not.is_false",
                    )
                    .map_err(|error| CompileError::Codegen(error.to_string()))?
            }
        };
        let tagged = builder
            .build_select(
                predicate,
                self.const_bool(true),
                self.const_bool(false),
                "not.tagged",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .into_int_value();
        Ok(CodegenValue::Word(tagged))
    }

    fn compile_numeric_comparison(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        callee: &str,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() < 2 {
            return Err(CompileError::Codegen(format!(
                "{callee} expects at least two arguments"
            )));
        }

        let mut decoded = Vec::with_capacity(args.len());
        for (index, expr) in args.iter().enumerate() {
            let value = self.compile_expr(builder, current_function, env, expr)?;
            let word = self.expect_word(value, "numeric comparison argument")?;
            let checked = self.ensure_fixnum(
                builder,
                current_function,
                word,
                &format!("num_cmp.{index}.fixnum.check"),
            )?;
            decoded.push(self.decode_fixnum(
                builder,
                checked,
                &format!("num_cmp.{index}.fixnum"),
            )?);
        }

        let predicate = match callee {
            "=" => IntPredicate::EQ,
            "<" => IntPredicate::SLT,
            ">" => IntPredicate::SGT,
            "<=" => IntPredicate::SLE,
            ">=" => IntPredicate::SGE,
            _ => unreachable!(),
        };

        let mut result = self.context.bool_type().const_int(1, false);
        for pair in decoded.windows(2) {
            let next = builder
                .build_int_compare(predicate, pair[0], pair[1], "num_cmp.step")
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            result = builder
                .build_and(result, next, "num_cmp.and")
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
        }

        Ok(CodegenValue::Word(
            builder
                .build_select(
                    result,
                    self.const_bool(true),
                    self.const_bool(false),
                    "num_cmp.tagged",
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?
                .into_int_value(),
        ))
    }

    fn compile_apply(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() < 2 {
            return Err(CompileError::Codegen(
                "apply expects a procedure, optional leading arguments, and a final list".into(),
            ));
        }

        let callee_value = self.compile_expr(builder, current_function, env, &args[0])?;
        let signature = match callee_value {
            CodegenValue::Function(info) => info.signature,
            CodegenValue::Closure(info) => info.signature,
            CodegenValue::Word(_)
            | CodegenValue::RootedWord { .. }
            | CodegenValue::HeapObject { .. }
            | CodegenValue::MutableBox { .. } => {
                return Err(CompileError::Codegen(
                    "apply target expected a function value, but a non-function was produced"
                        .into(),
                ));
            }
        };

        let prefix_args = &args[1..args.len() - 1];
        let list_arg = self.compile_expr(builder, current_function, env, args.last().unwrap())?;
        let list_word = self.value_to_word(builder, list_arg, "apply.list")?;

        let compiled_args = self.compile_apply_arguments(
            builder,
            current_function,
            env,
            signature,
            prefix_args,
            list_word,
        )?;
        self.emit_callable_call(builder, callee_value, signature, compiled_args)
    }

    fn compile_guard(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        name: &str,
        handler: &Expr,
        body: &Expr,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let body_block = self
            .context
            .append_basic_block(current_function, "guard.body");
        let handler_block = self
            .context
            .append_basic_block(current_function, "guard.handler");
        let merge_block = self
            .context
            .append_basic_block(current_function, "guard.merge");

        builder
            .build_unconditional_branch(body_block)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;

        builder.position_at_end(body_block);
        self.exception_targets.push(ExceptionTarget {
            block: handler_block,
        });
        let body_result = self.compile_expr(builder, current_function, env, body)?;
        self.exception_targets.pop();
        builder
            .build_unconditional_branch(merge_block)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let body_exit = builder
            .get_insert_block()
            .ok_or_else(|| CompileError::Codegen("missing guard body block".into()))?;

        builder.position_at_end(handler_block);
        let exception = builder
            .build_call(
                self.runtime.rt_take_pending_exception,
                &[],
                "guard.exception",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| {
                CompileError::Codegen("take-pending-exception did not return a value".into())
            })?
            .into_int_value();
        let mut guarded_env = env.clone();
        guarded_env.insert(name.to_string(), CodegenValue::Word(exception));
        let handler_result = self.compile_expr(builder, current_function, &guarded_env, handler)?;
        builder
            .build_unconditional_branch(merge_block)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let handler_exit = builder
            .get_insert_block()
            .ok_or_else(|| CompileError::Codegen("missing guard handler block".into()))?;

        builder.position_at_end(merge_block);
        self.merge_branch_values(
            builder,
            body_result,
            body_exit,
            handler_result,
            handler_exit,
            "guard.result",
        )
    }

    fn compile_values(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() == 1 {
            return self.compile_expr(builder, current_function, env, &args[0]);
        }

        let word_type = self.word_type();
        let raw_ptr_type = self.context.ptr_type(inkwell::AddressSpace::default());
        let mut rooted_slots = Vec::with_capacity(args.len());
        for (index, expr) in args.iter().enumerate() {
            let value = self.compile_expr(builder, current_function, env, expr)?;
            let word = self.value_to_word(builder, value, &format!("values.arg.{index}"))?;
            let CodegenValue::RootedWord { slot } =
                self.root_word(builder, word, &format!("values.arg.{index}.root"))?
            else {
                unreachable!()
            };
            rooted_slots.push(slot);
        }

        let values_ptr = if rooted_slots.is_empty() {
            raw_ptr_type.const_null()
        } else {
            let elements = builder
                .build_array_alloca(
                    word_type,
                    word_type.const_int(rooted_slots.len() as u64, false),
                    "values.elements",
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            for (index, slot) in rooted_slots.iter().enumerate() {
                let word =
                    self.load_rooted_word(builder, *slot, &format!("values.arg.{index}.load"))?;
                let element_slot = unsafe {
                    builder.build_gep(
                        word_type,
                        elements,
                        &[word_type.const_int(index as u64, false)],
                        &format!("values.element.{index}"),
                    )
                }
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
                builder
                    .build_store(element_slot, word)
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
            }
            builder
                .build_pointer_cast(elements, raw_ptr_type, "values.elements.raw")
                .map_err(|error| CompileError::Codegen(error.to_string()))?
        };

        let result = builder
            .build_call(
                self.runtime.alloc_values,
                &[
                    values_ptr.into(),
                    word_type.const_int(rooted_slots.len() as u64, false).into(),
                ],
                "values.packet",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| {
                CompileError::Codegen("values allocation did not return a value".into())
            })?
            .into_int_value();
        self.pop_root_slots(builder, rooted_slots.len())?;
        Ok(CodegenValue::Word(result))
    }

    fn compile_call_with_values(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 2 {
            return Err(CompileError::Codegen(
                "call-with-values expects exactly two arguments".into(),
            ));
        }

        let producer_value = self.compile_expr(builder, current_function, env, &args[0])?;
        let producer_signature = match producer_value {
            CodegenValue::Function(info) => info.signature,
            CodegenValue::Closure(info) => info.signature,
            _ => {
                return Err(CompileError::Codegen(
                    "call-with-values producer expected a function value".into(),
                ));
            }
        };
        if !producer_signature.required_param_kinds.is_empty() {
            return Err(CompileError::Codegen(
                "call-with-values producer must accept zero arguments".into(),
            ));
        }

        let consumer_value = self.compile_expr(builder, current_function, env, &args[1])?;
        let consumer_signature = match consumer_value {
            CodegenValue::Function(info) => info.signature,
            CodegenValue::Closure(info) => info.signature,
            _ => {
                return Err(CompileError::Codegen(
                    "call-with-values consumer expected a function value".into(),
                ));
            }
        };
        let rooted_consumer =
            self.root_callable_value(builder, consumer_value, "call_with_values.consumer")?;

        let produced =
            self.emit_callable_call(builder, producer_value, producer_signature, Vec::new())?;
        let produced_word = self.value_to_word(builder, produced, "call_with_values.produced")?;
        let CodegenValue::RootedWord {
            slot: produced_slot,
        } = self.root_word(builder, produced_word, "call_with_values.produced.root")?
        else {
            unreachable!()
        };

        let current_word =
            self.load_rooted_word(builder, produced_slot, "call_with_values.produced.current")?;
        let is_values = builder
            .build_call(
                self.runtime.is_values,
                &[current_word.into()],
                "call_with_values.is_values",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("is-values did not return a value".into()))?
            .into_int_value();

        let values_block = self
            .context
            .append_basic_block(current_function, "call_with_values.values");
        let single_block = self
            .context
            .append_basic_block(current_function, "call_with_values.single");
        let cont_block = self
            .context
            .append_basic_block(current_function, "call_with_values.cont");
        builder
            .build_conditional_branch(is_values, values_block, single_block)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;

        builder.position_at_end(values_block);
        let values_result = self.emit_call_with_values_packet_consumer(
            builder,
            current_function,
            rooted_consumer,
            consumer_signature,
            produced_slot,
        )?;
        let values_exit = builder
            .get_insert_block()
            .ok_or_else(|| CompileError::Codegen("missing values branch block".into()))?;
        builder
            .build_unconditional_branch(cont_block)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;

        builder.position_at_end(single_block);
        let single_result = self.emit_call_with_values_single_consumer(
            builder,
            current_function,
            rooted_consumer,
            consumer_signature,
            produced_slot,
        )?;
        let single_exit = if single_result.is_some() {
            let block = builder
                .get_insert_block()
                .ok_or_else(|| CompileError::Codegen("missing single-value branch block".into()))?;
            builder
                .build_unconditional_branch(cont_block)
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            Some(block)
        } else {
            None
        };

        builder.position_at_end(cont_block);
        let merged = match (single_result, single_exit) {
            (Some(single_result), Some(single_exit)) => self.merge_branch_values(
                builder,
                values_result,
                values_exit,
                single_result,
                single_exit,
                "call_with_values.result",
            )?,
            _ => values_result,
        };
        self.pop_root_slots(builder, 1)?;
        if matches!(rooted_consumer, RootedCallable::Closure { .. }) {
            self.pop_root_slots(builder, 1)?;
        }
        Ok(merged)
    }

    fn compile_raise(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "raise expects exactly one argument".into(),
            ));
        }
        let value = self.compile_expr(builder, current_function, env, &args[0])?;
        let word = self.value_to_word(builder, value, "raise.argument")?;
        let result = builder
            .build_call(self.runtime.rt_raise, &[word.into()], "raise")
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("raise did not return a value".into()))?
            .into_int_value();
        Ok(CodegenValue::Word(result))
    }

    fn compile_error(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.is_empty() {
            return Err(CompileError::Codegen(
                "error expects at least one argument".into(),
            ));
        }
        let mut items = Vec::with_capacity(args.len() + 1);
        let error_symbol = self.compile_symbol_literal(builder, "error")?;
        items.push(self.value_to_word(builder, error_symbol, "error.symbol")?);
        for (index, expr) in args.iter().enumerate() {
            let value = self.compile_expr(builder, current_function, env, expr)?;
            items.push(self.value_to_word(builder, value, &format!("error.arg.{index}"))?);
        }
        let error_word = self.build_list_word_from_words(builder, &items, "error.value")?;
        let result = builder
            .build_call(self.runtime.rt_raise, &[error_word.into()], "error.raise")
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("error did not return a value".into()))?
            .into_int_value();
        Ok(CodegenValue::Word(result))
    }

    fn compile_boolean_predicate(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "boolean? expects exactly one argument".into(),
            ));
        }
        let value = self.compile_expr(builder, current_function, env, &args[0])?;
        let result = match value {
            CodegenValue::HeapObject { .. }
            | CodegenValue::RootedWord { .. }
            | CodegenValue::MutableBox { .. }
            | CodegenValue::Function(_)
            | CodegenValue::Closure(_) => self.context.bool_type().const_zero(),
            CodegenValue::Word(word) => {
                let is_false = builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        word,
                        self.const_bool(false),
                        "boolean.is_false",
                    )
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
                let is_true = builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        word,
                        self.const_bool(true),
                        "boolean.is_true",
                    )
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
                builder
                    .build_or(is_false, is_true, "boolean.is_bool")
                    .map_err(|error| CompileError::Codegen(error.to_string()))?
            }
        };
        let tagged = builder
            .build_select(
                result,
                self.const_bool(true),
                self.const_bool(false),
                "boolean?.tagged",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .into_int_value();
        Ok(CodegenValue::Word(tagged))
    }

    fn compile_zero_predicate(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "zero? expects exactly one argument".into(),
            ));
        }
        let value = self.compile_expr(builder, current_function, env, &args[0])?;
        let word = self.expect_word(value, "zero? argument")?;
        let checked = self.ensure_fixnum(builder, current_function, word, "zero_predicate.arg")?;
        let decoded = self.decode_fixnum(builder, checked, "zero_predicate.decoded")?;
        let is_zero = builder
            .build_int_compare(
                IntPredicate::EQ,
                decoded,
                self.word_type().const_zero(),
                "zero_predicate.is_zero",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let tagged = builder
            .build_select(
                is_zero,
                self.const_bool(true),
                self.const_bool(false),
                "zero?.tagged",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .into_int_value();
        Ok(CodegenValue::Word(tagged))
    }

    fn compile_char_predicate(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "char? expects exactly one argument".into(),
            ));
        }
        let value = self.compile_expr(builder, current_function, env, &args[0])?;
        let word = self.expect_word(value, "char? argument")?;
        let mask = builder
            .build_and(
                word,
                self.word_type()
                    .const_int(crate::runtime::layout::IMMEDIATE_TAG_MASK as u64, false),
                "char.mask",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let is_char = builder
            .build_int_compare(
                IntPredicate::EQ,
                mask,
                self.word_type()
                    .const_int(crate::runtime::layout::CHAR_TAG as u64, false),
                "char.is_char",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let tagged = builder
            .build_select(
                is_char,
                self.const_bool(true),
                self.const_bool(false),
                "char?.tagged",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .into_int_value();
        Ok(CodegenValue::Word(tagged))
    }

    fn compile_char_comparison(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        callee: &str,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() < 2 {
            return Err(CompileError::Codegen(format!(
                "{callee} expects at least two arguments"
            )));
        }

        let mut decoded = Vec::with_capacity(args.len());
        for (index, arg) in args.iter().enumerate() {
            let value = self.compile_expr(builder, current_function, env, arg)?;
            let word = self.expect_word(value, "char comparison argument")?;
            let masked = builder
                .build_and(
                    word,
                    self.word_type()
                        .const_int(crate::runtime::layout::IMMEDIATE_TAG_MASK as u64, false),
                    &format!("char_cmp.{index}.mask"),
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            let is_char = builder
                .build_int_compare(
                    IntPredicate::EQ,
                    masked,
                    self.word_type()
                        .const_int(crate::runtime::layout::CHAR_TAG as u64, false),
                    &format!("char_cmp.{index}.is_char"),
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            self.trap_if_false(
                builder,
                current_function,
                is_char,
                &format!("char_cmp.{index}"),
            )?;
            decoded.push(
                builder
                    .build_right_shift(
                        word,
                        self.word_type()
                            .const_int(crate::runtime::layout::CHAR_SHIFT as u64, false),
                        false,
                        &format!("char_cmp.{index}.decoded"),
                    )
                    .map_err(|error| CompileError::Codegen(error.to_string()))?,
            );
        }

        let mut result = self.context.bool_type().const_int(1, false);
        for pair in decoded.windows(2) {
            let predicate = match callee {
                "char=?" => IntPredicate::EQ,
                "char<?" => IntPredicate::ULT,
                "char>?" => IntPredicate::UGT,
                "char<=?" => IntPredicate::ULE,
                "char>=?" => IntPredicate::UGE,
                _ => unreachable!(),
            };
            let next = builder
                .build_int_compare(predicate, pair[0], pair[1], "char_cmp.step")
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            result = builder
                .build_and(result, next, "char_cmp.and")
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
        }
        Ok(CodegenValue::Word(
            builder
                .build_select(
                    result,
                    self.const_bool(true),
                    self.const_bool(false),
                    "char_cmp.tagged",
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?
                .into_int_value(),
        ))
    }

    fn compile_char_to_integer(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "char->integer expects exactly one argument".into(),
            ));
        }
        let value = self.compile_expr(builder, current_function, env, &args[0])?;
        let word = self.expect_word(value, "char->integer argument")?;
        let masked = builder
            .build_and(
                word,
                self.word_type()
                    .const_int(crate::runtime::layout::IMMEDIATE_TAG_MASK as u64, false),
                "char_to_integer.mask",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let is_char = builder
            .build_int_compare(
                IntPredicate::EQ,
                masked,
                self.word_type()
                    .const_int(crate::runtime::layout::CHAR_TAG as u64, false),
                "char_to_integer.is_char",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        self.trap_if_false(builder, current_function, is_char, "char_to_integer")?;
        let decoded = builder
            .build_right_shift(
                word,
                self.word_type()
                    .const_int(crate::runtime::layout::CHAR_SHIFT as u64, false),
                false,
                "char_to_integer.decoded",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        Ok(CodegenValue::Word(self.encode_fixnum_value(
            builder,
            decoded,
            "char_to_integer.tagged",
        )?))
    }

    fn compile_integer_to_char(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "integer->char expects exactly one argument".into(),
            ));
        }
        let value = self.compile_expr(builder, current_function, env, &args[0])?;
        let word = self.expect_word(value, "integer->char argument")?;
        let checked = self.ensure_fixnum(builder, current_function, word, "integer_to_char.arg")?;
        let decoded = self.decode_fixnum(builder, checked, "integer_to_char.decoded")?;
        let max_scalar = builder
            .build_int_compare(
                IntPredicate::ULE,
                decoded,
                self.word_type().const_int(0x10FFFF, false),
                "integer_to_char.max",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let surrogate_lower = builder
            .build_int_compare(
                IntPredicate::UGE,
                decoded,
                self.word_type().const_int(0xD800, false),
                "integer_to_char.surrogate.lower",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let surrogate_upper = builder
            .build_int_compare(
                IntPredicate::ULE,
                decoded,
                self.word_type().const_int(0xDFFF, false),
                "integer_to_char.surrogate.upper",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let is_surrogate = builder
            .build_and(
                surrogate_lower,
                surrogate_upper,
                "integer_to_char.is_surrogate",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let valid_scalar = builder
            .build_not(is_surrogate, "integer_to_char.not_surrogate")
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let valid = builder
            .build_and(max_scalar, valid_scalar, "integer_to_char.valid")
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        self.trap_if_false(builder, current_function, valid, "integer_to_char.scalar")?;
        let shifted = builder
            .build_left_shift(
                decoded,
                self.word_type()
                    .const_int(crate::runtime::layout::CHAR_SHIFT as u64, false),
                "integer_to_char.shifted",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let tagged = builder
            .build_or(
                shifted,
                self.word_type()
                    .const_int(crate::runtime::layout::CHAR_TAG as u64, false),
                "integer_to_char.tagged",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        Ok(CodegenValue::Word(tagged))
    }

    fn compile_symbol_predicate(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "symbol? expects exactly one argument".into(),
            ));
        }
        let value = self.compile_expr(builder, current_function, env, &args[0])?;
        let result = match value {
            CodegenValue::HeapObject {
                kind: HeapValueKind::Symbol,
                ..
            } => self.context.bool_type().const_int(1, false),
            CodegenValue::HeapObject { .. }
            | CodegenValue::RootedWord { .. }
            | CodegenValue::MutableBox { .. }
            | CodegenValue::Function(_)
            | CodegenValue::Closure(_) => self.context.bool_type().const_zero(),
            other => {
                let word = self.value_to_word(builder, other, "symbol? argument")?;
                builder
                    .build_call(self.runtime.is_symbol, &[word.into()], "is_symbol")
                    .map_err(|error| CompileError::Codegen(error.to_string()))?
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| CompileError::Codegen("symbol? did not return a value".into()))?
                    .into_int_value()
            }
        };
        let tagged = builder
            .build_select(
                result,
                self.const_bool(true),
                self.const_bool(false),
                "symbol?.tagged",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .into_int_value();
        Ok(CodegenValue::Word(tagged))
    }

    fn compile_symbol_to_string(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "symbol->string expects exactly one argument".into(),
            ));
        }
        let value = self.compile_expr(builder, current_function, env, &args[0])?;
        let word = self.value_to_word(builder, value, "symbol->string argument")?;
        let result = builder
            .build_call(
                self.runtime.symbol_to_string,
                &[word.into()],
                "symbol_to_string",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("symbol->string did not return a value".into()))?
            .into_int_value();
        Ok(CodegenValue::Word(result))
    }

    fn compile_string_to_symbol(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "string->symbol expects exactly one argument".into(),
            ));
        }
        let value = self.compile_expr(builder, current_function, env, &args[0])?;
        let word = self.value_to_word(builder, value, "string->symbol argument")?;
        let result = builder
            .build_call(
                self.runtime.string_to_symbol,
                &[word.into()],
                "string_to_symbol",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("string->symbol did not return a value".into()))?
            .into_int_value();
        Ok(CodegenValue::Word(result))
    }

    fn compile_procedure_predicate(
        &mut self,
        _builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "procedure? expects exactly one argument".into(),
            ));
        }

        let value = self.compile_expr(_builder, current_function, env, &args[0])?;
        let is_procedure = match value {
            CodegenValue::Function(_) | CodegenValue::Closure(_) => true,
            CodegenValue::Word(_)
            | CodegenValue::RootedWord { .. }
            | CodegenValue::HeapObject { .. }
            | CodegenValue::MutableBox { .. } => false,
        };

        Ok(CodegenValue::Word(self.const_bool(is_procedure)))
    }

    fn compile_identity_predicate(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        callee: &str,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 2 {
            return Err(CompileError::Codegen(format!(
                "{callee} expects exactly two arguments"
            )));
        }

        let left = self.compile_expr(builder, current_function, env, &args[0])?;
        let right = self.compile_expr(builder, current_function, env, &args[1])?;
        let is_equal = self.compare_codegen_values(builder, left, right, callee)?;
        let tagged = builder
            .build_select(
                is_equal,
                self.const_bool(true),
                self.const_bool(false),
                &format!("{callee}.tagged"),
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .into_int_value();
        Ok(CodegenValue::Word(tagged))
    }

    fn compile_equal_predicate(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 2 {
            return Err(CompileError::Codegen(
                "equal? expects exactly two arguments".into(),
            ));
        }

        let left = self.compile_expr(builder, current_function, env, &args[0])?;
        let right = self.compile_expr(builder, current_function, env, &args[1])?;
        let left_word = self.value_to_word(builder, left, "equal?.left")?;
        let right_word = self.value_to_word(builder, right, "equal?.right")?;
        let result = builder
            .build_call(
                self.runtime.equal,
                &[left_word.into(), right_word.into()],
                "equal",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("equal? did not return a value".into()))?
            .into_int_value();
        let tagged = builder
            .build_select(
                result,
                self.const_bool(true),
                self.const_bool(false),
                "equal?.tagged",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .into_int_value();
        Ok(CodegenValue::Word(tagged))
    }

    fn compile_list(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let mut result = CodegenValue::Word(self.word_type().const_int(EMPTY_LIST as u64, false));
        for arg in args.iter().rev() {
            let car = self.compile_expr(builder, current_function, env, arg)?;
            let car_word = self.value_to_word(builder, car, "list.car")?;
            let cdr_word = self.value_to_word(builder, result, "list.cdr")?;
            let pair = self.alloc_pair_rooted(builder, car_word, cdr_word, "list.cons")?;
            result = CodegenValue::HeapObject {
                ptr: pair,
                kind: HeapValueKind::Pair,
            };
        }
        Ok(result)
    }

    fn compile_map(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        self.compile_list_iteration(builder, current_function, env, args, true)
    }

    fn compile_for_each(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        self.compile_list_iteration(builder, current_function, env, args, false)
    }

    fn compile_list_iteration(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
        collect_results: bool,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() < 2 {
            return Err(CompileError::Codegen(format!(
                "{} expects a procedure and at least one list",
                if collect_results { "map" } else { "for-each" }
            )));
        }

        let callee_value = self.compile_expr(builder, current_function, env, &args[0])?;
        let signature = match callee_value {
            CodegenValue::Function(info) => info.signature,
            CodegenValue::Closure(info) => info.signature,
            _ => {
                return Err(CompileError::Codegen(format!(
                    "{} target expected a function value",
                    if collect_results { "map" } else { "for-each" }
                )));
            }
        };

        let list_count = args.len() - 1;
        if list_count < signature.required_param_kinds.len()
            || (!signature.rest && list_count != signature.required_param_kinds.len())
        {
            return Err(CompileError::Codegen(format!(
                "procedure expects {}{} arguments but got {} list(s)",
                signature.required_param_kinds.len(),
                if signature.rest { " or more" } else { "" },
                list_count
            )));
        }

        let callable = self.root_callable_value(builder, callee_value, "list.iter.callable")?;

        let word_ptr_type = self.context.ptr_type(inkwell::AddressSpace::default());
        let zero = self.word_type().const_zero();
        let mut list_slots = Vec::with_capacity(list_count);
        let mut lengths = Vec::with_capacity(list_count);
        for (index, expr) in args[1..].iter().enumerate() {
            let list_value = self.compile_expr(builder, current_function, env, expr)?;
            let list_word =
                self.value_to_word(builder, list_value, &format!("list_iter.list.{index}"))?;
            let slot = builder
                .build_alloca(self.word_type(), &format!("list_iter.list.{index}.slot"))
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            builder
                .build_store(slot, list_word)
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            let raw_slot = builder
                .build_pointer_cast(slot, word_ptr_type, &format!("list_iter.list.{index}.raw"))
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            builder
                .build_call(self.runtime.rt_root_slot_push, &[raw_slot.into()], "")
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            list_slots.push(slot);
            let length = builder
                .build_call(
                    self.runtime.list_length,
                    &[list_word.into()],
                    &format!("list_iter.list.{index}.len"),
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?
                .try_as_basic_value()
                .basic()
                .ok_or_else(|| CompileError::Codegen("list-length did not return a value".into()))?
                .into_int_value();
            let checked = self.ensure_fixnum(
                builder,
                current_function,
                length,
                &format!("list_iter.list.{index}.len.fixnum"),
            )?;
            lengths.push(self.decode_fixnum(
                builder,
                checked,
                &format!("list_iter.list.{index}.len.decoded"),
            )?);
        }
        for length in lengths.iter().skip(1) {
            let same = builder
                .build_int_compare(
                    IntPredicate::EQ,
                    lengths[0],
                    *length,
                    "list_iter.same_length",
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            self.trap_if_false(builder, current_function, same, "list_iter.same_length")?;
        }

        let index_slot = builder
            .build_alloca(self.word_type(), "list_iter.index.slot")
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        builder
            .build_store(index_slot, zero)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;

        let result_slot = if collect_results {
            let slot = builder
                .build_alloca(self.word_type(), "list_iter.result.slot")
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            builder
                .build_store(slot, self.word_type().const_int(EMPTY_LIST as u64, false))
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            let raw_slot = builder
                .build_pointer_cast(slot, word_ptr_type, "list_iter.result.raw")
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            builder
                .build_call(self.runtime.rt_root_slot_push, &[raw_slot.into()], "")
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            Some(slot)
        } else {
            None
        };

        let cond_block = self
            .context
            .append_basic_block(current_function, "list_iter.cond");
        let body_block = self
            .context
            .append_basic_block(current_function, "list_iter.body");
        let step_block = self
            .context
            .append_basic_block(current_function, "list_iter.step");
        let done_block = self
            .context
            .append_basic_block(current_function, "list_iter.done");
        builder
            .build_unconditional_branch(cond_block)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;

        builder.position_at_end(cond_block);
        let index = builder
            .build_load(self.word_type(), index_slot, "list_iter.index")
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .into_int_value();
        let keep_going = builder
            .build_int_compare(IntPredicate::ULT, index, lengths[0], "list_iter.keep_going")
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        builder
            .build_conditional_branch(keep_going, body_block, done_block)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;

        builder.position_at_end(body_block);
        let mut iteration_args =
            Vec::with_capacity(signature.required_param_kinds.len() + usize::from(signature.rest));
        let direct_count = signature.required_param_kinds.len().min(list_slots.len());
        let mut extra_words = Vec::new();
        for (list_index, slot) in list_slots.iter().enumerate() {
            let list_word = builder
                .build_load(
                    self.word_type(),
                    *slot,
                    &format!("list_iter.list.{list_index}.load"),
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?
                .into_int_value();
            let value = builder
                .build_call(
                    self.runtime.list_ref,
                    &[list_word.into(), index.into()],
                    &format!("list_iter.arg.{list_index}"),
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?
                .try_as_basic_value()
                .basic()
                .ok_or_else(|| CompileError::Codegen("list-ref did not return a value".into()))?
                .into_int_value();
            if list_index < direct_count {
                let coerced = self.word_to_codegen_value(
                    builder,
                    value,
                    signature.required_param_kinds[list_index],
                    &format!("list_iter.arg.{list_index}.coerce"),
                )?;
                iteration_args.push(self.convert_argument_value(
                    builder,
                    coerced,
                    signature.required_param_kinds[list_index],
                    "list iteration arg",
                )?);
            } else {
                extra_words.push(value);
            }
        }
        if signature.rest {
            iteration_args.push(
                self.build_list_word_from_words(builder, &extra_words, "list_iter.rest")?
                    .into(),
            );
        }
        let call_result =
            self.emit_rooted_callable_call(builder, callable, signature, iteration_args)?;
        if let Some(slot) = result_slot {
            let item_word = self.value_to_word(builder, call_result, "map.result.item")?;
            let current = builder
                .build_load(self.word_type(), slot, "map.result.current")
                .map_err(|error| CompileError::Codegen(error.to_string()))?
                .into_int_value();
            let pair = self.alloc_pair_rooted(builder, item_word, current, "map.result.cons")?;
            let pair_word = builder
                .build_ptr_to_int(pair, self.word_type(), "map.result.word")
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            builder
                .build_store(slot, pair_word)
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
        }
        builder
            .build_unconditional_branch(step_block)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;

        builder.position_at_end(step_block);
        let next_index = builder
            .build_int_add(
                index,
                self.word_type().const_int(1, false),
                "list_iter.next",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        builder
            .build_store(index_slot, next_index)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        builder
            .build_unconditional_branch(cond_block)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;

        builder.position_at_end(done_block);
        let result = if let Some(slot) = result_slot {
            let reversed = builder
                .build_load(self.word_type(), slot, "map.result.reversed")
                .map_err(|error| CompileError::Codegen(error.to_string()))?
                .into_int_value();
            let final_result = builder
                .build_call(self.runtime.reverse, &[reversed.into()], "map.result")
                .map_err(|error| CompileError::Codegen(error.to_string()))?
                .try_as_basic_value()
                .basic()
                .ok_or_else(|| CompileError::Codegen("reverse did not return a value".into()))?
                .into_int_value();
            CodegenValue::Word(final_result)
        } else {
            CodegenValue::Word(
                self.word_type()
                    .const_int(Value::unspecified().bits() as u64, false),
            )
        };

        if collect_results {
            self.pop_root_slots(builder, 1)?;
        }
        self.pop_root_slots(builder, list_slots.len())?;
        if matches!(callable, RootedCallable::Closure { .. }) {
            self.pop_root_slots(builder, 1)?;
        }
        Ok(result)
    }

    fn compile_member_like(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
        target: FunctionValue<'ctx>,
        name: &str,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 2 {
            return Err(CompileError::Codegen(format!(
                "{name} expects exactly two arguments"
            )));
        }
        let key = self.compile_expr(builder, current_function, env, &args[0])?;
        let list = self.compile_expr(builder, current_function, env, &args[1])?;
        let key_word = self.value_to_word(builder, key, &format!("{name}.key"))?;
        let list_word = self.value_to_word(builder, list, &format!("{name}.list"))?;
        let result = builder
            .build_call(target, &[key_word.into(), list_word.into()], name)
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen(format!("{name} did not return a value")))?
            .into_int_value();
        Ok(CodegenValue::Word(result))
    }

    fn compile_append(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let mut result = if let Some(last) = args.last() {
            self.compile_expr(builder, current_function, env, last)?
        } else {
            CodegenValue::Word(self.word_type().const_int(EMPTY_LIST as u64, false))
        };

        for expr in args.iter().rev().skip(1) {
            let left = self.compile_expr(builder, current_function, env, expr)?;
            let left_word = self.value_to_word(builder, left, "append.left")?;
            let right_word = self.value_to_word(builder, result, "append.right")?;
            let appended = builder
                .build_call(
                    self.runtime.append,
                    &[left_word.into(), right_word.into()],
                    "append",
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?
                .try_as_basic_value()
                .basic()
                .ok_or_else(|| CompileError::Codegen("append did not return a value".into()))?;
            result = CodegenValue::Word(appended.into_int_value());
        }

        Ok(result)
    }

    fn compile_list_copy(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "list-copy expects exactly one argument".into(),
            ));
        }
        let value = self.compile_expr(builder, current_function, env, &args[0])?;
        let word = self.value_to_word(builder, value, "list-copy argument")?;
        let result = builder
            .build_call(self.runtime.list_copy, &[word.into()], "list_copy")
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("list-copy did not return a value".into()))?;
        Ok(CodegenValue::Word(result.into_int_value()))
    }

    fn compile_reverse(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "reverse expects exactly one argument".into(),
            ));
        }
        let value = self.compile_expr(builder, current_function, env, &args[0])?;
        let word = self.value_to_word(builder, value, "reverse argument")?;
        let result = builder
            .build_call(self.runtime.reverse, &[word.into()], "reverse")
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("reverse did not return a value".into()))?;
        Ok(CodegenValue::Word(result.into_int_value()))
    }

    fn compile_set(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        name: &str,
        value: &Expr,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let Some(binding) = env.get(name).copied() else {
            return Err(CompileError::Codegen(format!(
                "set! target '{name}' is undefined"
            )));
        };
        let CodegenValue::MutableBox { ptr } = binding else {
            return Err(CompileError::Codegen(format!(
                "set! target '{name}' is immutable in the current implementation"
            )));
        };
        let compiled = self.compile_expr(builder, current_function, env, value)?;
        let word = self.value_to_word(builder, compiled, "set! value")?;
        let result = builder
            .build_call(
                self.runtime.box_set_gc,
                &[ptr.into(), word.into()],
                "box.set",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("set! did not return a value".into()))?;
        Ok(CodegenValue::Word(result.into_int_value()))
    }

    fn compile_cons(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 2 {
            return Err(CompileError::Codegen(
                "cons expects exactly two arguments".into(),
            ));
        }

        let car_value = self.compile_expr(builder, current_function, env, &args[0])?;
        let car = self.value_to_word(builder, car_value, "cons.car")?;
        let cdr_value = self.compile_expr(builder, current_function, env, &args[1])?;
        let cdr = self.value_to_word(builder, cdr_value, "cons.cdr")?;
        Ok(CodegenValue::HeapObject {
            ptr: self.alloc_pair_rooted(builder, car, cdr, "cons.raw")?,
            kind: HeapValueKind::Pair,
        })
    }

    fn compile_pair_access(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
        car: bool,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(format!(
                "{} expects exactly one argument",
                if car { "car" } else { "cdr" }
            )));
        }

        let pair_value = self.compile_expr(builder, current_function, env, &args[0])?;
        let target = if car {
            self.runtime.pair_car
        } else {
            self.runtime.pair_cdr
        };
        let raw_target = if car {
            self.runtime.pair_car_gc
        } else {
            self.runtime.pair_cdr_gc
        };
        let result = match pair_value {
            CodegenValue::HeapObject {
                ptr: pair,
                kind: HeapValueKind::Pair,
            } => builder
                .build_call(
                    raw_target,
                    &[pair.into()],
                    if car { "car.raw" } else { "cdr.raw" },
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?
                .try_as_basic_value()
                .basic()
                .ok_or_else(|| {
                    CompileError::Codegen("pair access did not return a value".into())
                })?,
            other => {
                let pair =
                    self.expect_word(other, if car { "car argument" } else { "cdr argument" })?;
                builder
                    .build_call(target, &[pair.into()], if car { "car" } else { "cdr" })
                    .map_err(|error| CompileError::Codegen(error.to_string()))?
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| {
                        CompileError::Codegen("pair access did not return a value".into())
                    })?
            }
        };
        Ok(CodegenValue::Word(result.into_int_value()))
    }

    fn compile_pair_set(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
        set_car: bool,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 2 {
            return Err(CompileError::Codegen(format!(
                "{} expects exactly two arguments",
                if set_car { "set-car!" } else { "set-cdr!" }
            )));
        }
        let pair = if let ExprKind::Variable(name) = &args[0].kind {
            env.get(name)
                .copied()
                .map(Ok)
                .unwrap_or_else(|| self.compile_expr(builder, current_function, env, &args[0]))?
        } else {
            self.compile_expr(builder, current_function, env, &args[0])?
        };
        let value = self.compile_expr(builder, current_function, env, &args[1])?;
        let value_word = self.value_to_word(builder, value, "pair-set value")?;
        let result = match pair {
            CodegenValue::HeapObject {
                ptr,
                kind: HeapValueKind::Pair,
            } => builder
                .build_call(
                    if set_car {
                        self.runtime.pair_set_car_gc
                    } else {
                        self.runtime.pair_set_cdr_gc
                    },
                    &[ptr.into(), value_word.into()],
                    "pair_set_gc",
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?
                .try_as_basic_value()
                .basic()
                .ok_or_else(|| {
                    CompileError::Codegen("pair mutation did not return a value".into())
                })?,
            CodegenValue::RootedWord { slot } => {
                let pair_word = self.load_rooted_word(builder, slot, "pair_set.rooted")?;
                let pair_ptr = builder
                    .build_int_to_ptr(pair_word, gc_ptr_type(self.context), "pair_set.rooted.ptr")
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
                builder
                    .build_call(
                        if set_car {
                            self.runtime.pair_set_car_gc
                        } else {
                            self.runtime.pair_set_cdr_gc
                        },
                        &[pair_ptr.into(), value_word.into()],
                        "pair_set_gc",
                    )
                    .map_err(|error| CompileError::Codegen(error.to_string()))?
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| {
                        CompileError::Codegen("pair mutation did not return a value".into())
                    })?
            }
            other => {
                let pair_word = self.value_to_word(builder, other, "pair-set target")?;
                builder
                    .build_call(
                        if set_car {
                            self.runtime.pair_set_car
                        } else {
                            self.runtime.pair_set_cdr
                        },
                        &[pair_word.into(), value_word.into()],
                        "pair_set",
                    )
                    .map_err(|error| CompileError::Codegen(error.to_string()))?
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| {
                        CompileError::Codegen("pair mutation did not return a value".into())
                    })?
            }
        };
        Ok(CodegenValue::Word(result.into_int_value()))
    }

    fn compile_pair_predicate(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "pair? expects exactly one argument".into(),
            ));
        }

        let value_result = self.compile_expr(builder, current_function, env, &args[0])?;
        let result = match value_result {
            CodegenValue::HeapObject {
                kind: HeapValueKind::Pair,
                ..
            } => self.context.bool_type().const_int(1, false),
            CodegenValue::HeapObject { .. } => self.context.bool_type().const_zero(),
            other => {
                let value = self.expect_word(other, "pair? argument")?;
                builder
                    .build_call(self.runtime.is_pair, &[value.into()], "is_pair")
                    .map_err(|error| CompileError::Codegen(error.to_string()))?
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| CompileError::Codegen("pair? did not return a value".into()))?
                    .into_int_value()
            }
        };
        let tagged = builder
            .build_select(
                result,
                self.const_bool(true),
                self.const_bool(false),
                "pair?.tagged",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .into_int_value();
        Ok(CodegenValue::Word(tagged))
    }

    fn compile_list_predicate(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "list? expects exactly one argument".into(),
            ));
        }
        let value = self.compile_expr(builder, current_function, env, &args[0])?;
        let result = match value {
            CodegenValue::HeapObject {
                kind: HeapValueKind::Pair,
                ..
            } => {
                let word = self.value_to_word(builder, value, "list? argument")?;
                builder
                    .build_call(self.runtime.is_list, &[word.into()], "is_list")
                    .map_err(|error| CompileError::Codegen(error.to_string()))?
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| CompileError::Codegen("list? did not return a value".into()))?
                    .into_int_value()
            }
            CodegenValue::HeapObject { .. } | CodegenValue::Closure(_) => {
                self.context.bool_type().const_zero()
            }
            other => {
                let word = self.expect_word(other, "list? argument")?;
                builder
                    .build_call(self.runtime.is_list, &[word.into()], "is_list")
                    .map_err(|error| CompileError::Codegen(error.to_string()))?
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| CompileError::Codegen("list? did not return a value".into()))?
                    .into_int_value()
            }
        };
        let tagged = builder
            .build_select(
                result,
                self.const_bool(true),
                self.const_bool(false),
                "list?.tagged",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .into_int_value();
        Ok(CodegenValue::Word(tagged))
    }

    fn compile_list_length(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "length expects exactly one argument".into(),
            ));
        }
        let value = self.compile_expr(builder, current_function, env, &args[0])?;
        let word = self.value_to_word(builder, value, "length argument")?;
        let result = builder
            .build_call(self.runtime.list_length, &[word.into()], "list_length")
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("length did not return a value".into()))?;
        Ok(CodegenValue::Word(result.into_int_value()))
    }

    fn compile_list_tail(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 2 {
            return Err(CompileError::Codegen(
                "list-tail expects exactly two arguments".into(),
            ));
        }
        let list = self.compile_expr(builder, current_function, env, &args[0])?;
        let index = self.compile_expr(builder, current_function, env, &args[1])?;
        let list_word = self.value_to_word(builder, list, "list-tail argument")?;
        let index_word = self.expect_word(index, "list-tail index argument")?;
        let checked =
            self.ensure_fixnum(builder, current_function, index_word, "list_tail.index")?;
        let decoded = self.decode_fixnum(builder, checked, "list_tail.index.fixnum")?;
        let result = builder
            .build_call(
                self.runtime.list_tail,
                &[list_word.into(), decoded.into()],
                "list_tail",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("list-tail did not return a value".into()))?;
        Ok(CodegenValue::Word(result.into_int_value()))
    }

    fn compile_list_ref(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 2 {
            return Err(CompileError::Codegen(
                "list-ref expects exactly two arguments".into(),
            ));
        }
        let list = self.compile_expr(builder, current_function, env, &args[0])?;
        let index = self.compile_expr(builder, current_function, env, &args[1])?;
        let list_word = self.value_to_word(builder, list, "list-ref argument")?;
        let index_word = self.expect_word(index, "list-ref index argument")?;
        let checked =
            self.ensure_fixnum(builder, current_function, index_word, "list_ref.index")?;
        let decoded = self.decode_fixnum(builder, checked, "list_ref.index.fixnum")?;
        let result = builder
            .build_call(
                self.runtime.list_ref,
                &[list_word.into(), decoded.into()],
                "list_ref",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("list-ref did not return a value".into()))?;
        Ok(CodegenValue::Word(result.into_int_value()))
    }

    fn compile_null_predicate(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "null? expects exactly one argument".into(),
            ));
        }

        let value_result = self.compile_expr(builder, current_function, env, &args[0])?;
        let is_null = match value_result {
            CodegenValue::HeapObject { .. } => self.context.bool_type().const_zero(),
            other => {
                let value = self.expect_word(other, "null? argument")?;
                builder
                    .build_int_compare(
                        IntPredicate::EQ,
                        value,
                        self.word_type().const_int(EMPTY_LIST as u64, false),
                        "is_null",
                    )
                    .map_err(|error| CompileError::Codegen(error.to_string()))?
            }
        };
        let tagged = builder
            .build_select(
                is_null,
                self.const_bool(true),
                self.const_bool(false),
                "null?.tagged",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .into_int_value();
        Ok(CodegenValue::Word(tagged))
    }

    fn compile_string_predicate(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "string? expects exactly one argument".into(),
            ));
        }

        let value_result = self.compile_expr(builder, current_function, env, &args[0])?;
        let result = match value_result {
            CodegenValue::HeapObject {
                kind: HeapValueKind::String,
                ..
            } => self.context.bool_type().const_int(1, false),
            CodegenValue::HeapObject { .. } => self.context.bool_type().const_zero(),
            other => {
                let value = self.value_to_word(builder, other, "string? argument")?;
                builder
                    .build_call(self.runtime.is_string, &[value.into()], "is_string")
                    .map_err(|error| CompileError::Codegen(error.to_string()))?
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| CompileError::Codegen("string? did not return a value".into()))?
                    .into_int_value()
            }
        };
        let tagged = builder
            .build_select(
                result,
                self.const_bool(true),
                self.const_bool(false),
                "string?.tagged",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .into_int_value();
        Ok(CodegenValue::Word(tagged))
    }

    fn compile_string_length(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "string-length expects exactly one argument".into(),
            ));
        }
        let value = self.compile_expr(builder, current_function, env, &args[0])?;
        let result = match value {
            CodegenValue::HeapObject {
                ptr,
                kind: HeapValueKind::String,
            } => builder
                .build_call(
                    self.runtime.string_length_gc,
                    &[ptr.into()],
                    "string_length_gc",
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?
                .try_as_basic_value()
                .basic()
                .ok_or_else(|| {
                    CompileError::Codegen("string-length did not return a value".into())
                })?,
            other => {
                let word = self.value_to_word(builder, other, "string-length argument")?;
                builder
                    .build_call(self.runtime.string_length, &[word.into()], "string_length")
                    .map_err(|error| CompileError::Codegen(error.to_string()))?
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| {
                        CompileError::Codegen("string-length did not return a value".into())
                    })?
            }
        };
        Ok(CodegenValue::Word(result.into_int_value()))
    }

    fn compile_string_ref(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 2 {
            return Err(CompileError::Codegen(
                "string-ref expects exactly two arguments".into(),
            ));
        }

        let string_value = self.compile_expr(builder, current_function, env, &args[0])?;
        let index_value = self.compile_expr(builder, current_function, env, &args[1])?;
        let index_word = self.expect_word(index_value, "string-ref index argument")?;
        let checked_index =
            self.ensure_fixnum(builder, current_function, index_word, "string_ref.index")?;
        let index = self.decode_fixnum(builder, checked_index, "string_ref.index.fixnum")?;
        let result = match string_value {
            CodegenValue::HeapObject {
                ptr,
                kind: HeapValueKind::String,
            } => builder
                .build_call(
                    self.runtime.string_ref_gc,
                    &[ptr.into(), index.into()],
                    "string_ref_gc",
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?
                .try_as_basic_value()
                .basic()
                .ok_or_else(|| CompileError::Codegen("string-ref did not return a value".into()))?,
            other => {
                let string_word =
                    self.value_to_word(builder, other, "string-ref string argument")?;
                builder
                    .build_call(
                        self.runtime.string_ref,
                        &[string_word.into(), index.into()],
                        "string_ref",
                    )
                    .map_err(|error| CompileError::Codegen(error.to_string()))?
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| {
                        CompileError::Codegen("string-ref did not return a value".into())
                    })?
            }
        };
        Ok(CodegenValue::Word(result.into_int_value()))
    }

    fn compile_vector(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let word_type = self.word_type();
        let len = word_type.const_int(args.len() as u64, false);
        let raw_ptr_type = self.context.ptr_type(inkwell::AddressSpace::default());
        let elements_ptr = if args.is_empty() {
            raw_ptr_type.const_null()
        } else {
            let elements = builder
                .build_array_alloca(word_type, len, "vector.elements")
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            for (index, expr) in args.iter().enumerate() {
                let value = self.compile_expr(builder, current_function, env, expr)?;
                let word = self.value_to_word(builder, value, "vector element")?;
                let slot = unsafe {
                    builder.build_gep(
                        word_type,
                        elements,
                        &[word_type.const_int(index as u64, false)],
                        &format!("vector.element.{index}"),
                    )
                }
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
                builder
                    .build_store(slot, word)
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
            }
            builder
                .build_pointer_cast(elements, raw_ptr_type, "vector.elements.raw")
                .map_err(|error| CompileError::Codegen(error.to_string()))?
        };

        let result = builder
            .build_call(
                self.runtime.alloc_vector_gc,
                &[elements_ptr.into(), len.into()],
                "vector",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("vector did not return a value".into()))?;
        Ok(CodegenValue::HeapObject {
            ptr: result.into_pointer_value(),
            kind: HeapValueKind::Vector,
        })
    }

    fn compile_vector_predicate(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "vector? expects exactly one argument".into(),
            ));
        }

        let value = self.compile_expr(builder, current_function, env, &args[0])?;
        let result = match value {
            CodegenValue::HeapObject {
                kind: HeapValueKind::Vector,
                ..
            } => self.context.bool_type().const_int(1, false),
            CodegenValue::HeapObject { .. } => self.context.bool_type().const_zero(),
            other => {
                let word = self.value_to_word(builder, other, "vector? argument")?;
                builder
                    .build_call(self.runtime.is_vector, &[word.into()], "is_vector")
                    .map_err(|error| CompileError::Codegen(error.to_string()))?
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| CompileError::Codegen("vector? did not return a value".into()))?
                    .into_int_value()
            }
        };
        let tagged = builder
            .build_select(
                result,
                self.const_bool(true),
                self.const_bool(false),
                "vector?.tagged",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .into_int_value();
        Ok(CodegenValue::Word(tagged))
    }

    fn compile_vector_length(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "vector-length expects exactly one argument".into(),
            ));
        }
        let value = self.compile_expr(builder, current_function, env, &args[0])?;
        let result = match value {
            CodegenValue::HeapObject {
                ptr,
                kind: HeapValueKind::Vector,
            } => builder
                .build_call(
                    self.runtime.vector_length_gc,
                    &[ptr.into()],
                    "vector_length_gc",
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?
                .try_as_basic_value()
                .basic()
                .ok_or_else(|| {
                    CompileError::Codegen("vector-length did not return a value".into())
                })?,
            other => {
                let word = self.value_to_word(builder, other, "vector-length argument")?;
                builder
                    .build_call(self.runtime.vector_length, &[word.into()], "vector_length")
                    .map_err(|error| CompileError::Codegen(error.to_string()))?
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| {
                        CompileError::Codegen("vector-length did not return a value".into())
                    })?
            }
        };
        Ok(CodegenValue::Word(result.into_int_value()))
    }

    fn compile_vector_ref(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 2 {
            return Err(CompileError::Codegen(
                "vector-ref expects exactly two arguments".into(),
            ));
        }
        let vector_value = self.compile_expr(builder, current_function, env, &args[0])?;
        let index_value = self.compile_expr(builder, current_function, env, &args[1])?;
        let index_word = self.expect_word(index_value, "vector-ref index argument")?;
        let checked_index =
            self.ensure_fixnum(builder, current_function, index_word, "vector_ref.index")?;
        let index = self.decode_fixnum(builder, checked_index, "vector_ref.index.fixnum")?;
        let result = match vector_value {
            CodegenValue::HeapObject {
                ptr,
                kind: HeapValueKind::Vector,
            } => builder
                .build_call(
                    self.runtime.vector_ref_gc,
                    &[ptr.into(), index.into()],
                    "vector_ref_gc",
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?
                .try_as_basic_value()
                .basic()
                .ok_or_else(|| CompileError::Codegen("vector-ref did not return a value".into()))?,
            other => {
                let vector_word =
                    self.value_to_word(builder, other, "vector-ref vector argument")?;
                builder
                    .build_call(
                        self.runtime.vector_ref,
                        &[vector_word.into(), index.into()],
                        "vector_ref",
                    )
                    .map_err(|error| CompileError::Codegen(error.to_string()))?
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| {
                        CompileError::Codegen("vector-ref did not return a value".into())
                    })?
            }
        };
        Ok(CodegenValue::Word(result.into_int_value()))
    }

    fn compile_vector_set(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 3 {
            return Err(CompileError::Codegen(
                "vector-set! expects exactly three arguments".into(),
            ));
        }
        let vector_value = self.compile_expr(builder, current_function, env, &args[0])?;
        let index_value = self.compile_expr(builder, current_function, env, &args[1])?;
        let index_word = self.expect_word(index_value, "vector-set! index argument")?;
        let checked_index =
            self.ensure_fixnum(builder, current_function, index_word, "vector_set.index")?;
        let index = self.decode_fixnum(builder, checked_index, "vector_set.index.fixnum")?;
        let element_value = self.compile_expr(builder, current_function, env, &args[2])?;
        let element_word = self.value_to_word(builder, element_value, "vector-set! element")?;
        let result = match vector_value {
            CodegenValue::HeapObject {
                ptr,
                kind: HeapValueKind::Vector,
            } => builder
                .build_call(
                    self.runtime.vector_set_gc,
                    &[ptr.into(), index.into(), element_word.into()],
                    "vector_set_gc",
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?
                .try_as_basic_value()
                .basic()
                .ok_or_else(|| {
                    CompileError::Codegen("vector-set! did not return a value".into())
                })?,
            other => {
                let vector_word =
                    self.value_to_word(builder, other, "vector-set! vector argument")?;
                builder
                    .build_call(
                        self.runtime.vector_set,
                        &[vector_word.into(), index.into(), element_word.into()],
                        "vector_set",
                    )
                    .map_err(|error| CompileError::Codegen(error.to_string()))?
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| {
                        CompileError::Codegen("vector-set! did not return a value".into())
                    })?
            }
        };
        Ok(CodegenValue::Word(result.into_int_value()))
    }

    fn compile_display(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "display expects exactly one argument".into(),
            ));
        }
        let value = self.compile_expr(builder, current_function, env, &args[0])?;
        let word = self.value_to_word(builder, value, "display argument")?;
        let result = builder
            .build_call(self.runtime.display, &[word.into()], "display")
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("display did not return a value".into()))?;
        Ok(CodegenValue::Word(result.into_int_value()))
    }

    fn compile_gc_stress(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "gc-stress expects exactly one argument".into(),
            ));
        }
        let iterations = self.compile_expr(builder, current_function, env, &args[0])?;
        let word = self.expect_word(iterations, "gc-stress iterations")?;
        let checked =
            self.ensure_fixnum(builder, current_function, word, "gc_stress.iterations")?;
        let decoded = self.decode_fixnum(builder, checked, "gc_stress.iterations.fixnum")?;
        let result = builder
            .build_call(self.runtime.gc_stress, &[decoded.into()], "gc_stress")
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("gc-stress did not return a value".into()))?;
        Ok(CodegenValue::Word(result.into_int_value()))
    }

    fn compile_write(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if args.len() != 1 {
            return Err(CompileError::Codegen(
                "write expects exactly one argument".into(),
            ));
        }
        let value = self.compile_expr(builder, current_function, env, &args[0])?;
        let word = self.value_to_word(builder, value, "write argument")?;
        let result = builder
            .build_call(self.runtime.write, &[word.into()], "write")
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("write did not return a value".into()))?;
        Ok(CodegenValue::Word(result.into_int_value()))
    }

    fn compile_newline(
        &mut self,
        builder: &Builder<'ctx>,
        _current_function: FunctionValue<'ctx>,
        _env: &HashMap<String, CodegenValue<'ctx>>,
        args: &[Expr],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        if !args.is_empty() {
            return Err(CompileError::Codegen("newline expects no arguments".into()));
        }
        let result = builder
            .build_call(self.runtime.newline, &[], "newline")
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("newline did not return a value".into()))?;
        Ok(CodegenValue::Word(result.into_int_value()))
    }

    fn compile_quote(
        &mut self,
        builder: &Builder<'ctx>,
        datum: &Datum,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        match datum {
            Datum::Integer(value) => Ok(CodegenValue::Word(self.const_fixnum_checked(*value)?)),
            Datum::Boolean(value) => Ok(CodegenValue::Word(self.const_bool(*value))),
            Datum::Char(value) => Ok(CodegenValue::Word(
                self.word_type()
                    .const_int(Value::encode_char(*value).bits() as u64, false),
            )),
            Datum::String(value) => self.compile_string_literal(builder, value),
            Datum::Symbol(value) => self.compile_symbol_literal(builder, value),
            Datum::List { items, tail } if items.is_empty() && tail.is_none() => Ok(
                CodegenValue::Word(self.word_type().const_int(EMPTY_LIST as u64, false)),
            ),
            Datum::List { items, tail } => {
                let mut result = match tail {
                    Some(tail) => self.compile_quote(builder, tail)?,
                    None => {
                        CodegenValue::Word(self.word_type().const_int(EMPTY_LIST as u64, false))
                    }
                };
                for item in items.iter().rev() {
                    let car = self.compile_quote(builder, item)?;
                    let car_word = self.value_to_word(builder, car, "quoted list car")?;
                    let cdr_word = self.value_to_word(builder, result, "quoted list cdr")?;
                    result = CodegenValue::HeapObject {
                        ptr: self.alloc_pair_rooted(builder, car_word, cdr_word, "quote.cons")?,
                        kind: HeapValueKind::Pair,
                    };
                }
                Ok(result)
            }
        }
    }

    fn compile_parallel_bindings(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        bindings: &[Binding],
        body: &Expr,
    ) -> Result<(HashMap<String, CodegenValue<'ctx>>, usize), CompileError> {
        let mutables = self.collect_mutated_names_with_initial(
            body,
            bindings.iter().map(|binding| binding.name.clone()),
        );
        let pair_mutated = self.collect_pair_mutated_names_with_initial(
            body,
            bindings.iter().map(|binding| binding.name.clone()),
        );
        let mut bound = Vec::with_capacity(bindings.len());
        let mut rooted_count = 0usize;
        for binding in bindings {
            let value = self.compile_expr(builder, current_function, env, &binding.value)?;
            let stored = if mutables.contains(&binding.name) {
                self.box_value(builder, value, &format!("let.{}.box", binding.name))?
            } else if pair_mutated.contains(&binding.name) {
                rooted_count += 1;
                self.root_word(
                    builder,
                    self.value_to_word(builder, value, &format!("let.{}.word", binding.name))?,
                    &format!("let.{}.root", binding.name),
                )?
            } else {
                value
            };
            bound.push((binding.name.clone(), stored));
        }

        let mut scoped = env.clone();
        scoped.extend(bound);
        Ok((scoped, rooted_count))
    }

    fn compile_sequential_bindings(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        bindings: &[Binding],
        body: &Expr,
    ) -> Result<(HashMap<String, CodegenValue<'ctx>>, usize), CompileError> {
        let mutables = self.collect_mutated_names_with_initial(
            body,
            bindings.iter().map(|binding| binding.name.clone()),
        );
        let pair_mutated = self.collect_pair_mutated_names_with_initial(
            body,
            bindings.iter().map(|binding| binding.name.clone()),
        );
        let mut scoped = env.clone();
        let mut rooted_count = 0usize;
        for binding in bindings {
            let value = self.compile_expr(builder, current_function, &scoped, &binding.value)?;
            let stored = if mutables.contains(&binding.name) {
                self.box_value(builder, value, &format!("let_star.{}.box", binding.name))?
            } else if pair_mutated.contains(&binding.name) {
                rooted_count += 1;
                self.root_word(
                    builder,
                    self.value_to_word(builder, value, &format!("let_star.{}.word", binding.name))?,
                    &format!("let_star.{}.root", binding.name),
                )?
            } else {
                value
            };
            scoped.insert(binding.name.clone(), stored);
        }
        Ok((scoped, rooted_count))
    }

    fn compile_recursive_bindings(
        &mut self,
        builder: &Builder<'ctx>,
        _current_function: FunctionValue<'ctx>,
        outer_env: &HashMap<String, CodegenValue<'ctx>>,
        bindings: &[Binding],
    ) -> Result<HashMap<String, CodegenValue<'ctx>>, CompileError> {
        let mut binding_env = HashMap::new();
        for (name, value) in outer_env {
            let kind = match value {
                CodegenValue::Word(_)
                | CodegenValue::RootedWord { .. }
                | CodegenValue::MutableBox { .. } => BindingKind::Value(AbiValueKind::Word),
                CodegenValue::HeapObject { kind, .. } => {
                    BindingKind::Value(AbiValueKind::Heap(*kind))
                }
                CodegenValue::Function(info) => BindingKind::Function(info.signature),
                CodegenValue::Closure(info) => BindingKind::Function(info.signature),
            };
            binding_env.insert(name.clone(), kind);
        }
        let function_signatures =
            self.infer_letrec_function_signatures_with_env(&binding_env, bindings);
        let mut provisional_env = outer_env.clone();
        let mut declared = Vec::new();

        for binding in bindings {
            let ExprKind::Lambda { formals, body } = &binding.value.kind else {
                return Err(CompileError::Codegen(
                    "letrec currently supports only lambda bindings".into(),
                ));
            };

            let function_name = format!(
                "__letrec_{}_{}",
                sanitize_name(&binding.name),
                self.lambda_counter
            );
            self.lambda_counter += 1;
            let signature = function_signatures
                .get(&binding.name)
                .copied()
                .unwrap_or_else(|| self.default_signature(formals));
            let function = self.declare_closure_function(&function_name, signature);
            let wrapper = self.module.add_function(
                &format!("__scheme_wrap_{function_name}"),
                self.scheme_wrapper_type(),
                None,
            );
            attach_gc_strategy(wrapper);
            provisional_env.insert(
                binding.name.clone(),
                CodegenValue::Closure(ClosureInfo {
                    ptr: gc_ptr_type(self.context).const_null(),
                    signature,
                }),
            );
            declared.push((
                binding.name.clone(),
                function,
                wrapper,
                signature,
                formals.clone(),
                (**body).clone(),
            ));
        }

        let mut captures_by_name: HashMap<String, Vec<(String, CaptureKind)>> = HashMap::new();
        let mut env = HashMap::new();
        for (name, _function, wrapper, signature, formals, body) in &declared {
            let captures = self.collect_captures(&provisional_env, formals, body)?;
            let closure_value =
                self.allocate_placeholder_closure(builder, *wrapper, *signature, captures.len())?;
            provisional_env.insert(name.clone(), closure_value);
            env.insert(name.clone(), closure_value);
            captures_by_name.insert(name.clone(), captures);
        }

        for (name, _function, _wrapper, _signature, _params, _body) in &declared {
            let closure = match env.get(name).copied() {
                Some(CodegenValue::Closure(info)) => info,
                _ => {
                    return Err(CompileError::Codegen(format!(
                        "missing letrec closure binding '{}'",
                        name
                    )));
                }
            };
            let captures = captures_by_name.get(name).ok_or_else(|| {
                CompileError::Codegen(format!("missing letrec capture plan for '{}'", name))
            })?;
            for (index, (capture_name, _kind)) in captures.iter().enumerate() {
                let value = provisional_env.get(capture_name).copied().ok_or_else(|| {
                    CompileError::Codegen(format!(
                        "missing captured value '{}' for letrec binding '{}'",
                        capture_name, name
                    ))
                })?;
                let word = self.value_to_word(builder, value, "letrec capture")?;
                builder
                    .build_call(
                        self.runtime.closure_env_set_gc,
                        &[
                            closure.ptr.into(),
                            self.word_type().const_int(index as u64, false).into(),
                            word.into(),
                        ],
                        "letrec.capture.set",
                    )
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
            }
        }

        for (name, function, wrapper, signature, formals, body) in declared {
            let captures = captures_by_name.remove(&name).ok_or_else(|| {
                CompileError::Codegen(format!("missing letrec captures for '{}'", name))
            })?;
            self.compile_closure_body(function, signature, &formals, &captures, &body)?;
            self.compile_closure_scheme_wrapper(wrapper, function, signature, &formals)?;
        }

        Ok(env)
    }

    fn compile_lambda(
        &mut self,
        builder: &Builder<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        formals: &Formals,
        body: &Expr,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let function_name = format!("__lambda_{}", self.lambda_counter);
        self.lambda_counter += 1;
        let signature = self.infer_lambda_signature_from_values(env, formals, body);
        let captures = self.collect_captures(env, formals, body)?;
        if captures.is_empty() {
            let function = self.declare_lambda_function(&function_name, signature);
            let wrapper = self.module.add_function(
                &format!("__scheme_wrap_{function_name}"),
                self.scheme_wrapper_type(),
                None,
            );
            attach_gc_strategy(wrapper);
            let info = FunctionInfo {
                value: function,
                wrapper,
                signature,
            };
            self.compile_function_body(info, formals, body, &HashMap::new())?;
            self.compile_direct_scheme_wrapper(info, formals)?;
            Ok(CodegenValue::Function(info))
        } else {
            let function = self.declare_closure_function(&function_name, signature);
            let wrapper = self.module.add_function(
                &format!("__scheme_wrap_{function_name}"),
                self.scheme_wrapper_type(),
                None,
            );
            attach_gc_strategy(wrapper);
            self.compile_closure_body(function, signature, formals, &captures, body)?;
            self.compile_closure_scheme_wrapper(wrapper, function, signature, formals)?;
            self.compile_closure_allocation(builder, env, wrapper, signature, &captures)
        }
    }

    fn declare_lambda_function(
        &mut self,
        name: &str,
        signature: FunctionSignature,
    ) -> FunctionValue<'ctx> {
        let function = self
            .module
            .add_function(name, self.function_type(signature), None);
        attach_gc_strategy(function);
        function
    }

    fn declare_closure_function(
        &mut self,
        name: &str,
        signature: FunctionSignature,
    ) -> FunctionValue<'ctx> {
        let function = self
            .module
            .add_function(name, self.closure_function_type(signature), None);
        attach_gc_strategy(function);
        function
    }

    fn word_type(&self) -> inkwell::types::IntType<'ctx> {
        self.context.i64_type()
    }

    fn function_type(&self, signature: FunctionSignature) -> inkwell::types::FunctionType<'ctx> {
        let gc_ptr = gc_ptr_type(self.context);
        let params = signature
            .required_param_kinds
            .iter()
            .map(|kind| match kind {
                AbiValueKind::Word => self.word_type().into(),
                AbiValueKind::Heap(_) => gc_ptr.into(),
            })
            .chain(signature.rest.then_some(self.word_type().into()))
            .collect::<Vec<_>>();
        match signature.return_kind {
            AbiValueKind::Word => self.word_type().fn_type(&params, false),
            AbiValueKind::Heap(_) => gc_ptr.fn_type(&params, false),
        }
    }

    fn closure_function_type(
        &self,
        signature: FunctionSignature,
    ) -> inkwell::types::FunctionType<'ctx> {
        let gc_ptr = gc_ptr_type(self.context);
        let mut params: Vec<inkwell::types::BasicMetadataTypeEnum<'ctx>> = Vec::with_capacity(
            signature.required_param_kinds.len() + 1 + usize::from(signature.rest),
        );
        params.push(gc_ptr.into());
        for kind in signature.required_param_kinds {
            params.push(match kind {
                AbiValueKind::Word => self.word_type().into(),
                AbiValueKind::Heap(_) => gc_ptr.into(),
            });
        }
        if signature.rest {
            params.push(self.word_type().into());
        }
        match signature.return_kind {
            AbiValueKind::Word => self.word_type().fn_type(&params, false),
            AbiValueKind::Heap(_) => gc_ptr.fn_type(&params, false),
        }
    }

    fn scheme_wrapper_type(&self) -> inkwell::types::FunctionType<'ctx> {
        self.word_type().fn_type(
            &[gc_ptr_type(self.context).into(), self.word_type().into()],
            false,
        )
    }

    fn load_closure_code_ptr(
        &self,
        builder: &Builder<'ctx>,
        closure_ptr: PointerValue<'ctx>,
        name: &str,
    ) -> Result<IntValue<'ctx>, CompileError> {
        let slot = unsafe {
            builder.build_gep(
                self.word_type(),
                closure_ptr,
                &[self.word_type().const_int(2, false)],
                &format!("{name}.slot"),
            )
        }
        .map_err(|error| CompileError::Codegen(error.to_string()))?;
        builder
            .build_load(self.word_type(), slot, name)
            .map_err(|error| CompileError::Codegen(error.to_string()))
            .map(|value| value.into_int_value())
    }

    fn load_closure_env_word(
        &self,
        builder: &Builder<'ctx>,
        closure_ptr: PointerValue<'ctx>,
        index: usize,
        name: &str,
    ) -> Result<IntValue<'ctx>, CompileError> {
        let slot = unsafe {
            builder.build_gep(
                self.word_type(),
                closure_ptr,
                &[self.word_type().const_int((4 + index) as u64, false)],
                &format!("{name}.slot"),
            )
        }
        .map_err(|error| CompileError::Codegen(error.to_string()))?;
        builder
            .build_load(self.word_type(), slot, name)
            .map_err(|error| CompileError::Codegen(error.to_string()))
            .map(|value| value.into_int_value())
    }

    fn load_box_word(
        &self,
        builder: &Builder<'ctx>,
        box_ptr: PointerValue<'ctx>,
        name: &str,
    ) -> Result<IntValue<'ctx>, CompileError> {
        let slot = unsafe {
            builder.build_gep(
                self.word_type(),
                box_ptr,
                &[self.word_type().const_int(2, false)],
                &format!("{name}.slot"),
            )
        }
        .map_err(|error| CompileError::Codegen(error.to_string()))?;
        builder
            .build_load(self.word_type(), slot, name)
            .map_err(|error| CompileError::Codegen(error.to_string()))
            .map(|value| value.into_int_value())
    }

    fn load_rooted_word(
        &self,
        builder: &Builder<'ctx>,
        slot: PointerValue<'ctx>,
        name: &str,
    ) -> Result<IntValue<'ctx>, CompileError> {
        builder
            .build_load(self.word_type(), slot, name)
            .map_err(|error| CompileError::Codegen(error.to_string()))
            .map(|value| value.into_int_value())
    }

    fn root_word(
        &self,
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        name: &str,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let slot = builder
            .build_alloca(self.word_type(), name)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        builder
            .build_store(slot, value)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let raw_ptr = builder
            .build_pointer_cast(
                slot,
                self.context.ptr_type(inkwell::AddressSpace::default()),
                &format!("{name}.raw"),
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        builder
            .build_call(
                self.runtime.rt_root_slot_push,
                &[raw_ptr.into()],
                &format!("{name}.push"),
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        Ok(CodegenValue::RootedWord { slot })
    }

    fn pop_root_slots(&self, builder: &Builder<'ctx>, count: usize) -> Result<(), CompileError> {
        for _ in 0..count {
            builder
                .build_call(self.runtime.rt_root_slot_pop, &[], "root.pop")
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
        }
        Ok(())
    }

    fn box_value(
        &self,
        builder: &Builder<'ctx>,
        value: CodegenValue<'ctx>,
        name: &str,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let word = self.value_to_word(builder, value, name)?;
        let ptr = builder
            .build_call(self.runtime.alloc_box_gc, &[word.into()], name)
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("box allocation did not return a value".into()))?
            .into_pointer_value();
        Ok(CodegenValue::MutableBox { ptr })
    }

    fn build_list_value(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        exprs: &[Expr],
        name: &str,
    ) -> Result<IntValue<'ctx>, CompileError> {
        let mut result = self.word_type().const_int(EMPTY_LIST as u64, false);
        for (index, expr) in exprs.iter().enumerate().rev() {
            let value = self.compile_expr(builder, current_function, env, expr)?;
            let car = self.value_to_word(builder, value, &format!("{name}.item.{index}"))?;
            let pair =
                self.alloc_pair_rooted(builder, car, result, &format!("{name}.cons.{index}"))?;
            result = builder
                .build_ptr_to_int(pair, self.word_type(), &format!("{name}.word.{index}"))
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
        }
        Ok(result)
    }

    fn build_prefixed_list_value(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        exprs: &[Expr],
        tail: IntValue<'ctx>,
        name: &str,
    ) -> Result<IntValue<'ctx>, CompileError> {
        let mut result = tail;
        for (index, expr) in exprs.iter().enumerate().rev() {
            let value = self.compile_expr(builder, current_function, env, expr)?;
            let car = self.value_to_word(builder, value, &format!("{name}.item.{index}"))?;
            let pair =
                self.alloc_pair_rooted(builder, car, result, &format!("{name}.cons.{index}"))?;
            result = builder
                .build_ptr_to_int(pair, self.word_type(), &format!("{name}.word.{index}"))
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
        }
        Ok(result)
    }

    fn alloc_pair_rooted(
        &self,
        builder: &Builder<'ctx>,
        car: IntValue<'ctx>,
        cdr: IntValue<'ctx>,
        name: &str,
    ) -> Result<PointerValue<'ctx>, CompileError> {
        let rooted_car = self.root_word(builder, car, &format!("{name}.car.root"))?;
        let rooted_cdr = self.root_word(builder, cdr, &format!("{name}.cdr.root"))?;
        let car_word = self.value_to_word(builder, rooted_car, &format!("{name}.car"))?;
        let cdr_word = self.value_to_word(builder, rooted_cdr, &format!("{name}.cdr"))?;
        let call = builder
            .build_call(
                self.runtime.alloc_pair_gc,
                &[car_word.into(), cdr_word.into()],
                name,
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let result = call
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("pair allocation did not return a value".into()))?
            .into_pointer_value();
        self.pop_root_slots(builder, 2)?;
        Ok(result)
    }

    fn build_list_word_from_words(
        &self,
        builder: &Builder<'ctx>,
        values: &[IntValue<'ctx>],
        name: &str,
    ) -> Result<IntValue<'ctx>, CompileError> {
        let mut result = self.word_type().const_int(EMPTY_LIST as u64, false);
        for (index, value) in values.iter().enumerate().rev() {
            let pair =
                self.alloc_pair_rooted(builder, *value, result, &format!("{name}.cons.{index}"))?;
            result = builder
                .build_ptr_to_int(pair, self.word_type(), &format!("{name}.word.{index}"))
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
        }
        Ok(result)
    }

    fn metadata_values_to_words(
        &self,
        builder: &Builder<'ctx>,
        values: &[BasicMetadataValueEnum<'ctx>],
        name: &str,
    ) -> Result<Vec<IntValue<'ctx>>, CompileError> {
        values
            .iter()
            .enumerate()
            .map(|(index, value)| match value {
                BasicMetadataValueEnum::IntValue(int) => Ok(*int),
                BasicMetadataValueEnum::PointerValue(ptr) => builder
                    .build_ptr_to_int(*ptr, self.word_type(), &format!("{name}.ptr.{index}"))
                    .map_err(|error| CompileError::Codegen(error.to_string())),
                other => Err(CompileError::Codegen(format!(
                    "unsupported Scheme argument metadata value: {other:?}"
                ))),
            })
            .collect::<Result<Vec<_>, _>>()
    }

    fn build_prefixed_list_word(
        &self,
        builder: &Builder<'ctx>,
        prefix: &[IntValue<'ctx>],
        tail: IntValue<'ctx>,
        name: &str,
    ) -> Result<IntValue<'ctx>, CompileError> {
        let mut result = tail;
        for (index, value) in prefix.iter().enumerate().rev() {
            let pair =
                self.alloc_pair_rooted(builder, *value, result, &format!("{name}.cons.{index}"))?;
            result = builder
                .build_ptr_to_int(pair, self.word_type(), &format!("{name}.word.{index}"))
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
        }
        Ok(result)
    }

    fn build_scheme_args_list(
        &self,
        builder: &Builder<'ctx>,
        signature: FunctionSignature,
        values: &[BasicMetadataValueEnum<'ctx>],
        name: &str,
    ) -> Result<IntValue<'ctx>, CompileError> {
        let words = self.metadata_values_to_words(builder, values, name)?;
        let required = signature.required_param_kinds.len();
        if !signature.rest {
            if words.len() != required {
                return Err(CompileError::Codegen(format!(
                    "non-variadic callable expected {required} worker arguments but got {}",
                    words.len()
                )));
            }
            return self.build_list_word_from_words(builder, &words, name);
        }
        if words.len() != required + 1 {
            return Err(CompileError::Codegen(format!(
                "variadic callable expected {} worker arguments but got {}",
                required + 1,
                words.len()
            )));
        }
        self.build_prefixed_list_word(builder, &words[..required], words[required], name)
    }

    fn root_callable_value(
        &self,
        builder: &Builder<'ctx>,
        callee_value: CodegenValue<'ctx>,
        name: &str,
    ) -> Result<RootedCallable<'ctx>, CompileError> {
        match callee_value {
            CodegenValue::Function(info) => Ok(RootedCallable::Function(info)),
            CodegenValue::Closure(info) => {
                let word = builder
                    .build_ptr_to_int(info.ptr, self.word_type(), &format!("{name}.word"))
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
                let rooted = self.root_word(builder, word, &format!("{name}.root"))?;
                let CodegenValue::RootedWord { slot } = rooted else {
                    unreachable!()
                };
                Ok(RootedCallable::Closure {
                    slot,
                    signature: info.signature,
                })
            }
            _ => Err(CompileError::Codegen("expected a callable value".into())),
        }
    }

    fn emit_rooted_callable_call(
        &self,
        builder: &Builder<'ctx>,
        callee_value: RootedCallable<'ctx>,
        signature: FunctionSignature,
        compiled_args: Vec<BasicMetadataValueEnum<'ctx>>,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        match callee_value {
            RootedCallable::Function(info) => self.emit_callable_call(
                builder,
                CodegenValue::Function(info),
                signature,
                compiled_args,
            ),
            RootedCallable::Closure { slot, signature } => {
                let closure_word =
                    self.load_rooted_word(builder, slot, "rooted_callable.closure")?;
                let ptr = builder
                    .build_int_to_ptr(
                        closure_word,
                        gc_ptr_type(self.context),
                        "rooted_callable.closure.ptr",
                    )
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
                self.emit_callable_call(
                    builder,
                    CodegenValue::Closure(ClosureInfo { ptr, signature }),
                    signature,
                    compiled_args,
                )
            }
        }
    }

    fn compile_apply_arguments(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        signature: FunctionSignature,
        prefix_args: &[Expr],
        list_word: IntValue<'ctx>,
    ) -> Result<Vec<BasicMetadataValueEnum<'ctx>>, CompileError> {
        let required = signature.required_param_kinds.len();
        if !signature.rest && prefix_args.len() > required {
            return Err(CompileError::Codegen(format!(
                "apply received too many leading arguments for a procedure of arity {}",
                required
            )));
        }

        let mut compiled = Vec::with_capacity(required + usize::from(signature.rest));
        let direct_from_prefix = prefix_args.len().min(required);
        for (expr, kind) in prefix_args
            .iter()
            .take(direct_from_prefix)
            .zip(signature.required_param_kinds.iter())
        {
            let value = self.compile_expr(builder, current_function, env, expr)?;
            compiled.push(self.convert_argument_value(builder, value, *kind, "apply.prefix")?);
        }

        let needed_from_list = required.saturating_sub(direct_from_prefix);
        if !signature.rest {
            self.assert_list_length(builder, current_function, list_word, needed_from_list)?;
        } else if prefix_args.len() <= required {
            self.assert_list_length_at_least(
                builder,
                current_function,
                list_word,
                needed_from_list,
            )?;
        }

        for index in 0..needed_from_list {
            let value = self.load_list_element(
                builder,
                list_word,
                index,
                signature.required_param_kinds[direct_from_prefix + index],
                "apply.list.arg",
            )?;
            compiled.push(self.convert_argument_value(
                builder,
                value,
                signature.required_param_kinds[direct_from_prefix + index],
                "apply.list.arg",
            )?);
        }

        if signature.rest {
            let rest_list = if prefix_args.len() > required {
                self.build_prefixed_list_value(
                    builder,
                    current_function,
                    env,
                    &prefix_args[required..],
                    list_word,
                    "apply.rest.prefix",
                )?
            } else {
                self.list_tail_word(builder, list_word, needed_from_list, "apply.rest.tail")?
            };
            compiled.push(rest_list.into());
        }

        Ok(compiled)
    }

    fn emit_call_with_values_packet_consumer(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        consumer: RootedCallable<'ctx>,
        signature: FunctionSignature,
        produced_slot: PointerValue<'ctx>,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let produced_word =
            self.load_rooted_word(builder, produced_slot, "call_with_values.packet.word")?;
        if !signature.rest {
            self.assert_values_length(
                builder,
                current_function,
                produced_word,
                signature.required_param_kinds.len(),
            )?;
        } else {
            self.assert_values_length_at_least(
                builder,
                current_function,
                produced_word,
                signature.required_param_kinds.len(),
            )?;
        }

        let mut compiled_args =
            Vec::with_capacity(signature.required_param_kinds.len() + usize::from(signature.rest));
        for (index, kind) in signature.required_param_kinds.iter().enumerate() {
            let value = self.load_values_element(
                builder,
                produced_word,
                index,
                *kind,
                "call_with_values.packet.arg",
            )?;
            compiled_args.push(self.convert_argument_value(
                builder,
                value,
                *kind,
                "call-with-values packet arg",
            )?);
        }

        if signature.rest {
            let rest = builder
                .build_call(
                    self.runtime.values_tail_list,
                    &[
                        produced_word.into(),
                        self.word_type()
                            .const_int(signature.required_param_kinds.len() as u64, false)
                            .into(),
                    ],
                    "call_with_values.packet.rest",
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?
                .try_as_basic_value()
                .basic()
                .ok_or_else(|| {
                    CompileError::Codegen("values-tail-list did not return a value".into())
                })?
                .into_int_value();
            compiled_args.push(rest.into());
        }

        self.emit_rooted_callable_call(builder, consumer, signature, compiled_args)
    }

    fn emit_call_with_values_single_consumer(
        &self,
        builder: &Builder<'ctx>,
        _current_function: FunctionValue<'ctx>,
        consumer: RootedCallable<'ctx>,
        signature: FunctionSignature,
        produced_slot: PointerValue<'ctx>,
    ) -> Result<Option<CodegenValue<'ctx>>, CompileError> {
        let valid = signature.required_param_kinds.len() <= 1
            && (signature.rest || signature.required_param_kinds.len() == 1);
        if !valid {
            builder
                .build_call(self.trap_intrinsic(), &[], "")
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            builder
                .build_unreachable()
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            return Ok(None);
        }

        let produced_word =
            self.load_rooted_word(builder, produced_slot, "call_with_values.single.word")?;
        let mut compiled_args =
            Vec::with_capacity(signature.required_param_kinds.len() + usize::from(signature.rest));

        if let Some(kind) = signature.required_param_kinds.first().copied() {
            let coerced = self.word_to_codegen_value(
                builder,
                produced_word,
                kind,
                "call_with_values.single.arg.coerce",
            )?;
            compiled_args.push(self.convert_argument_value(
                builder,
                coerced,
                kind,
                "call-with-values single arg",
            )?);
        }

        if signature.rest {
            let rest_list = if signature.required_param_kinds.is_empty() {
                self.build_list_word_from_words(
                    builder,
                    &[produced_word],
                    "call_with_values.single.rest",
                )?
            } else {
                self.word_type().const_int(EMPTY_LIST as u64, false)
            };
            compiled_args.push(rest_list.into());
        }

        self.emit_rooted_callable_call(builder, consumer, signature, compiled_args)
            .map(Some)
    }

    fn convert_argument_value(
        &self,
        builder: &Builder<'ctx>,
        value: CodegenValue<'ctx>,
        kind: AbiValueKind,
        context: &str,
    ) -> Result<BasicMetadataValueEnum<'ctx>, CompileError> {
        match kind {
            AbiValueKind::Word => self
                .value_to_word(builder, value, context)
                .map(BasicMetadataValueEnum::from),
            AbiValueKind::Heap(heap_kind) => self
                .expect_heap_object(value, heap_kind, context)
                .map(BasicMetadataValueEnum::from),
        }
    }

    fn word_to_codegen_value(
        &self,
        builder: &Builder<'ctx>,
        word: IntValue<'ctx>,
        kind: AbiValueKind,
        name: &str,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        Ok(match kind {
            AbiValueKind::Word => CodegenValue::Word(word),
            AbiValueKind::Heap(heap_kind) => CodegenValue::HeapObject {
                ptr: builder
                    .build_int_to_ptr(word, gc_ptr_type(self.context), name)
                    .map_err(|error| CompileError::Codegen(error.to_string()))?,
                kind: heap_kind,
            },
        })
    }

    fn load_list_element(
        &self,
        builder: &Builder<'ctx>,
        list_word: IntValue<'ctx>,
        index: usize,
        kind: AbiValueKind,
        name: &str,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let result = builder
            .build_call(
                self.runtime.list_ref,
                &[
                    list_word.into(),
                    self.word_type().const_int(index as u64, false).into(),
                ],
                name,
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("list-ref did not return a value".into()))?
            .into_int_value();
        self.word_to_codegen_value(builder, result, kind, &format!("{name}.coerce"))
    }

    fn load_values_element(
        &self,
        builder: &Builder<'ctx>,
        values_word: IntValue<'ctx>,
        index: usize,
        kind: AbiValueKind,
        name: &str,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let result = builder
            .build_call(
                self.runtime.values_ref,
                &[
                    values_word.into(),
                    self.word_type().const_int(index as u64, false).into(),
                ],
                name,
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("values-ref did not return a value".into()))?
            .into_int_value();
        self.word_to_codegen_value(builder, result, kind, &format!("{name}.coerce"))
    }

    fn list_tail_word(
        &self,
        builder: &Builder<'ctx>,
        list_word: IntValue<'ctx>,
        index: usize,
        name: &str,
    ) -> Result<IntValue<'ctx>, CompileError> {
        builder
            .build_call(
                self.runtime.list_tail,
                &[
                    list_word.into(),
                    self.word_type().const_int(index as u64, false).into(),
                ],
                name,
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("list-tail did not return a value".into()))
            .map(|value| value.into_int_value())
    }

    fn assert_list_length(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        list_word: IntValue<'ctx>,
        expected: usize,
    ) -> Result<(), CompileError> {
        let length = builder
            .build_call(
                self.runtime.list_length,
                &[list_word.into()],
                "apply.length",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("list-length did not return a value".into()))?
            .into_int_value();
        let checked =
            self.ensure_fixnum(builder, current_function, length, "apply.length.fixnum")?;
        let decoded = self.decode_fixnum(builder, checked, "apply.length.decoded")?;
        let predicate = builder
            .build_int_compare(
                IntPredicate::EQ,
                decoded,
                self.word_type().const_int(expected as u64, false),
                "apply.length.matches",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        self.trap_if_false(builder, current_function, predicate, "apply.length")
    }

    fn assert_list_length_at_least(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        list_word: IntValue<'ctx>,
        minimum: usize,
    ) -> Result<(), CompileError> {
        let length = builder
            .build_call(
                self.runtime.list_length,
                &[list_word.into()],
                "apply.length",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("list-length did not return a value".into()))?
            .into_int_value();
        let checked =
            self.ensure_fixnum(builder, current_function, length, "apply.length.fixnum")?;
        let decoded = self.decode_fixnum(builder, checked, "apply.length.decoded")?;
        let predicate = builder
            .build_int_compare(
                IntPredicate::UGE,
                decoded,
                self.word_type().const_int(minimum as u64, false),
                "apply.length.at_least",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        self.trap_if_false(builder, current_function, predicate, "apply.length")
    }

    fn assert_values_length(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        values_word: IntValue<'ctx>,
        expected: usize,
    ) -> Result<(), CompileError> {
        let length = builder
            .build_call(
                self.runtime.values_length,
                &[values_word.into()],
                "call_with_values.length",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("values-length did not return a value".into()))?
            .into_int_value();
        let checked = self.ensure_fixnum(
            builder,
            current_function,
            length,
            "call_with_values.length.fixnum",
        )?;
        let decoded = self.decode_fixnum(builder, checked, "call_with_values.length.decoded")?;
        let predicate = builder
            .build_int_compare(
                IntPredicate::EQ,
                decoded,
                self.word_type().const_int(expected as u64, false),
                "call_with_values.length.matches",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        self.trap_if_false(
            builder,
            current_function,
            predicate,
            "call_with_values.length",
        )
    }

    fn assert_values_length_at_least(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        values_word: IntValue<'ctx>,
        minimum: usize,
    ) -> Result<(), CompileError> {
        let length = builder
            .build_call(
                self.runtime.values_length,
                &[values_word.into()],
                "call_with_values.length",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("values-length did not return a value".into()))?
            .into_int_value();
        let checked = self.ensure_fixnum(
            builder,
            current_function,
            length,
            "call_with_values.length.fixnum",
        )?;
        let decoded = self.decode_fixnum(builder, checked, "call_with_values.length.decoded")?;
        let predicate = builder
            .build_int_compare(
                IntPredicate::UGE,
                decoded,
                self.word_type().const_int(minimum as u64, false),
                "call_with_values.length.at_least",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        self.trap_if_false(
            builder,
            current_function,
            predicate,
            "call_with_values.length",
        )
    }

    fn trap_if_false(
        &self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        predicate: IntValue<'ctx>,
        name: &str,
    ) -> Result<(), CompileError> {
        let ok_block = self
            .context
            .append_basic_block(current_function, &format!("{name}.ok"));
        let trap_block = self
            .context
            .append_basic_block(current_function, &format!("{name}.trap"));
        builder
            .build_conditional_branch(predicate, ok_block, trap_block)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        builder.position_at_end(trap_block);
        builder
            .build_call(self.trap_intrinsic(), &[], "")
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        builder
            .build_unreachable()
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        builder.position_at_end(ok_block);
        Ok(())
    }

    fn branch_on_pending_exception(
        &self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        value: CodegenValue<'ctx>,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let Some(target) = self.exception_targets.last().copied() else {
            return Ok(value);
        };
        let pending = builder
            .build_call(self.runtime.rt_exception_pending, &[], "exception.pending")
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| {
                CompileError::Codegen("exception-pending did not return a value".into())
            })?
            .into_int_value();
        let cont = self
            .context
            .append_basic_block(current_function, "exception.cont");
        builder
            .build_conditional_branch(pending, target.block, cont)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        builder.position_at_end(cont);
        Ok(value)
    }

    fn scheme_callable_parts(
        &self,
        builder: &Builder<'ctx>,
        callee_value: CodegenValue<'ctx>,
    ) -> Result<(IntValue<'ctx>, IntValue<'ctx>, FunctionSignature), CompileError> {
        match callee_value {
            CodegenValue::Function(info) => {
                let code_ptr = builder
                    .build_ptr_to_int(
                        info.wrapper.as_global_value().as_pointer_value(),
                        self.word_type(),
                        "scheme.callable.code",
                    )
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
                Ok((code_ptr, self.word_type().const_zero(), info.signature))
            }
            CodegenValue::Closure(info) => {
                let code_ptr =
                    self.load_closure_code_ptr(builder, info.ptr, "scheme.callable.code")?;
                let closure_word = builder
                    .build_ptr_to_int(info.ptr, self.word_type(), "scheme.callable.closure")
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
                Ok((code_ptr, closure_word, info.signature))
            }
            _ => Err(CompileError::Codegen(
                "call target expected a function value, but a non-function was produced".into(),
            )),
        }
    }

    fn emit_callable_call(
        &self,
        builder: &Builder<'ctx>,
        callee_value: CodegenValue<'ctx>,
        signature: FunctionSignature,
        compiled_args: Vec<BasicMetadataValueEnum<'ctx>>,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let args_list =
            self.build_scheme_args_list(builder, signature, &compiled_args, "call.args")?;
        self.emit_trampoline_call(builder, callee_value, signature, args_list)
    }

    fn emit_trampoline_call(
        &self,
        builder: &Builder<'ctx>,
        callee_value: CodegenValue<'ctx>,
        signature: FunctionSignature,
        args_list: IntValue<'ctx>,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let (code_ptr, closure_word, callee_signature) =
            self.scheme_callable_parts(builder, callee_value)?;
        if callee_signature != signature {
            return Err(CompileError::Codegen(
                "mismatched callable signature during trampoline call".into(),
            ));
        }
        let result = builder
            .build_call(
                self.runtime.rt_trampoline_apply,
                &[code_ptr.into(), closure_word.into(), args_list.into()],
                "trampoline.call",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("trampoline call did not return a value".into()))?
            .into_int_value();
        self.word_to_codegen_value(
            builder,
            result,
            signature.return_kind,
            "trampoline.call.result",
        )
    }

    fn emit_tail_request(
        &self,
        builder: &Builder<'ctx>,
        return_kind: AbiValueKind,
        callee_value: CodegenValue<'ctx>,
        signature: FunctionSignature,
        args_list: IntValue<'ctx>,
        cleanup_roots: usize,
    ) -> Result<(), CompileError> {
        let (code_ptr, closure_word, callee_signature) =
            self.scheme_callable_parts(builder, callee_value)?;
        if callee_signature != signature {
            return Err(CompileError::Codegen(
                "mismatched callable signature during tail request".into(),
            ));
        }
        self.pop_root_slots(builder, cleanup_roots)?;
        builder
            .build_call(
                self.runtime.rt_tail_invoke,
                &[code_ptr.into(), closure_word.into(), args_list.into()],
                "tail.invoke",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        match return_kind {
            AbiValueKind::Word => {
                let marker = builder
                    .build_call(self.runtime.rt_tail_call_marker, &[], "tail.marker")
                    .map_err(|error| CompileError::Codegen(error.to_string()))?
                    .try_as_basic_value()
                    .basic()
                    .ok_or_else(|| {
                        CompileError::Codegen("tail marker did not return a value".into())
                    })?
                    .into_int_value();
                builder
                    .build_return(Some(&marker))
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
            }
            AbiValueKind::Heap(_) => {
                let null = gc_ptr_type(self.context).const_null();
                builder
                    .build_return(Some(&null))
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
            }
        }
        Ok(())
    }

    fn tail_return_value(
        &self,
        builder: &Builder<'ctx>,
        cleanup_roots: usize,
        return_kind: AbiValueKind,
        value: CodegenValue<'ctx>,
    ) -> Result<(), CompileError> {
        match return_kind {
            AbiValueKind::Word => {
                let return_value = self.value_to_word(builder, value, "tail.return")?;
                self.pop_root_slots(builder, cleanup_roots)?;
                builder
                    .build_return(Some(&return_value))
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
                Ok(())
            }
            AbiValueKind::Heap(kind) => {
                let return_value = self.expect_heap_object(value, kind, "tail.return")?;
                self.pop_root_slots(builder, cleanup_roots)?;
                builder
                    .build_return(Some(&return_value))
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
                Ok(())
            }
        }
    }

    fn emit_exception_return_block(
        &self,
        exception_block: BasicBlock<'ctx>,
        return_kind: AbiValueKind,
        rooted_slots: usize,
    ) -> Result<(), CompileError> {
        let builder = self.context.create_builder();
        if exception_block.get_first_instruction().is_some() {
            return Ok(());
        }
        builder.position_at_end(exception_block);
        for _ in 0..rooted_slots {
            builder
                .build_call(self.runtime.rt_root_slot_pop, &[], "exception.root.pop")
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
        }
        match return_kind {
            AbiValueKind::Word => {
                let value = self
                    .word_type()
                    .const_int(Value::unspecified().bits() as u64, false);
                builder
                    .build_return(Some(&value))
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
            }
            AbiValueKind::Heap(_) => {
                let value = gc_ptr_type(self.context).const_null();
                builder
                    .build_return(Some(&value))
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
            }
        }
        Ok(())
    }

    fn default_signature(&self, formals: &Formals) -> FunctionSignature {
        let param_kinds = vec![AbiValueKind::Word; formals.required.len()].into_boxed_slice();
        FunctionSignature {
            return_kind: AbiValueKind::Word,
            required_param_kinds: Box::leak(param_kinds),
            rest: formals.rest.is_some(),
        }
    }

    fn dynamic_callable_signature(&self, arity: usize) -> FunctionSignature {
        let param_kinds = vec![AbiValueKind::Word; arity].into_boxed_slice();
        FunctionSignature {
            return_kind: AbiValueKind::Word,
            required_param_kinds: Box::leak(param_kinds),
            rest: false,
        }
    }

    fn resolve_callable_value(
        &self,
        builder: &Builder<'ctx>,
        value: CodegenValue<'ctx>,
        arg_count: usize,
    ) -> Result<(CodegenValue<'ctx>, FunctionSignature), CompileError> {
        match value {
            CodegenValue::Function(info) => Ok((CodegenValue::Function(info), info.signature)),
            CodegenValue::Closure(info) => Ok((CodegenValue::Closure(info), info.signature)),
            CodegenValue::Word(word) => {
                let signature = self.dynamic_callable_signature(arg_count);
                let ptr = builder
                    .build_int_to_ptr(word, gc_ptr_type(self.context), "dynamic.callable.ptr")
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
                Ok((
                    CodegenValue::Closure(ClosureInfo { ptr, signature }),
                    signature,
                ))
            }
            CodegenValue::RootedWord { slot } => {
                let signature = self.dynamic_callable_signature(arg_count);
                let word = self.load_rooted_word(builder, slot, "dynamic.callable.word")?;
                let ptr = builder
                    .build_int_to_ptr(word, gc_ptr_type(self.context), "dynamic.callable.ptr")
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
                Ok((
                    CodegenValue::Closure(ClosureInfo { ptr, signature }),
                    signature,
                ))
            }
            CodegenValue::HeapObject { .. } | CodegenValue::MutableBox { .. } => {
                Err(CompileError::Codegen(
                    "call target expected a function value, but a non-function was produced".into(),
                ))
            }
        }
    }

    fn infer_top_level_function_signatures(
        &self,
        program: &Program,
    ) -> HashMap<String, FunctionSignature> {
        let mut functions = HashMap::new();
        for item in &program.items {
            if let TopLevel::Procedure(procedure) = item {
                let mut env = HashMap::new();
                let param_kinds =
                    self.infer_param_kinds(&procedure.formals.required, &procedure.body);
                for (param, kind) in procedure
                    .formals
                    .required
                    .iter()
                    .zip(param_kinds.iter().copied())
                {
                    env.insert(param.clone(), BindingKind::Value(kind));
                }
                if let Some(rest) = &procedure.formals.rest {
                    env.insert(rest.clone(), BindingKind::Value(AbiValueKind::Word));
                }
                let signature = FunctionSignature {
                    return_kind: self.infer_expr_kind(&procedure.body, &env, &functions),
                    required_param_kinds: Box::leak(param_kinds.into_boxed_slice()),
                    rest: procedure.formals.rest.is_some(),
                };
                functions.insert(procedure.name.clone(), signature);
            }
        }
        functions
    }

    fn infer_letrec_function_signatures(
        &self,
        bindings: &[Binding],
    ) -> HashMap<String, FunctionSignature> {
        self.infer_letrec_function_signatures_with_env(&HashMap::new(), bindings)
    }

    fn infer_letrec_function_signatures_with_env(
        &self,
        outer_env: &HashMap<String, BindingKind>,
        bindings: &[Binding],
    ) -> HashMap<String, FunctionSignature> {
        let mut functions = HashMap::new();
        for binding in bindings {
            if let ExprKind::Lambda { formals, .. } = &binding.value.kind {
                functions.insert(binding.name.clone(), self.default_signature(formals));
            }
        }

        for _ in 0..bindings.len().max(1) {
            let mut next = functions.clone();
            for binding in bindings {
                let ExprKind::Lambda {
                    formals: params,
                    body,
                } = &binding.value.kind
                else {
                    continue;
                };
                let mut env = outer_env.clone();
                for other in bindings {
                    if matches!(other.value.kind, ExprKind::Lambda { .. }) {
                        let signature = functions.get(&other.name).copied().unwrap_or_else(|| {
                            self.default_signature(&Formals {
                                required: Vec::new(),
                                rest: None,
                            })
                        });
                        env.insert(other.name.clone(), BindingKind::Function(signature));
                    }
                }
                next.insert(
                    binding.name.clone(),
                    self.infer_lambda_signature_with_env(params, body, &env, &functions),
                );
            }
            if next == functions {
                break;
            }
            functions = next;
        }

        functions
    }

    fn infer_lambda_signature_from_values(
        &self,
        value_env: &HashMap<String, CodegenValue<'ctx>>,
        formals: &Formals,
        body: &Expr,
    ) -> FunctionSignature {
        let mut binding_env: HashMap<String, BindingKind> = HashMap::new();
        for (name, value) in value_env {
            let kind = match value {
                CodegenValue::Word(_)
                | CodegenValue::RootedWord { .. }
                | CodegenValue::MutableBox { .. } => BindingKind::Value(AbiValueKind::Word),
                CodegenValue::HeapObject { kind, .. } => {
                    BindingKind::Value(AbiValueKind::Heap(*kind))
                }
                CodegenValue::Function(info) => BindingKind::Function(info.signature),
                CodegenValue::Closure(info) => BindingKind::Function(info.signature),
            };
            binding_env.insert(name.clone(), kind);
        }
        self.infer_lambda_signature_with_env(formals, body, &binding_env, &HashMap::new())
    }

    fn infer_lambda_signature_with_env(
        &self,
        formals: &Formals,
        body: &Expr,
        outer_env: &HashMap<String, BindingKind>,
        functions: &HashMap<String, FunctionSignature>,
    ) -> FunctionSignature {
        let mut env = outer_env.clone();
        let param_kinds = self.infer_param_kinds(&formals.required, body);
        for (param, kind) in formals.required.iter().zip(param_kinds.iter().copied()) {
            env.insert(param.clone(), BindingKind::Value(kind));
        }
        if let Some(rest) = &formals.rest {
            env.insert(rest.clone(), BindingKind::Value(AbiValueKind::Word));
        }
        FunctionSignature {
            return_kind: self.infer_expr_kind(body, &env, functions),
            required_param_kinds: Box::leak(param_kinds.into_boxed_slice()),
            rest: formals.rest.is_some(),
        }
    }

    fn infer_expr_kind(
        &self,
        expr: &Expr,
        env: &HashMap<String, BindingKind>,
        functions: &HashMap<String, FunctionSignature>,
    ) -> AbiValueKind {
        match &expr.kind {
            ExprKind::Unspecified
            | ExprKind::Integer(_)
            | ExprKind::Boolean(_)
            | ExprKind::Char(_) => AbiValueKind::Word,
            ExprKind::Quote(datum) => self.infer_quote_kind(datum),
            ExprKind::String(_) => AbiValueKind::Heap(HeapValueKind::String),
            ExprKind::Variable(name) => match env.get(name) {
                Some(BindingKind::Value(kind)) => *kind,
                Some(BindingKind::Function(_)) => AbiValueKind::Word,
                None => AbiValueKind::Word,
            },
            ExprKind::Set { .. } => AbiValueKind::Word,
            ExprKind::Begin(exprs) => exprs
                .last()
                .map(|expr| self.infer_expr_kind(expr, env, functions))
                .unwrap_or(AbiValueKind::Word),
            ExprKind::Let { bindings, body } => {
                let mut scoped = env.clone();
                for binding in bindings {
                    if let ExprKind::Lambda {
                        formals: params,
                        body,
                    } = &binding.value.kind
                    {
                        scoped.insert(
                            binding.name.clone(),
                            BindingKind::Function(
                                self.infer_lambda_signature_with_env(params, body, env, functions),
                            ),
                        );
                    } else {
                        let kind = self.infer_expr_kind(&binding.value, env, functions);
                        scoped.insert(binding.name.clone(), BindingKind::Value(kind));
                    }
                }
                self.infer_expr_kind(body, &scoped, functions)
            }
            ExprKind::LetStar { bindings, body } => {
                let mut scoped = env.clone();
                for binding in bindings {
                    if let ExprKind::Lambda {
                        formals: params,
                        body,
                    } = &binding.value.kind
                    {
                        scoped.insert(
                            binding.name.clone(),
                            BindingKind::Function(
                                self.infer_lambda_signature_with_env(
                                    params, body, &scoped, functions,
                                ),
                            ),
                        );
                    } else {
                        let kind = self.infer_expr_kind(&binding.value, &scoped, functions);
                        scoped.insert(binding.name.clone(), BindingKind::Value(kind));
                    }
                }
                self.infer_expr_kind(body, &scoped, functions)
            }
            ExprKind::LetRec { bindings, body } => {
                let mut scoped = env.clone();
                let function_signatures = self.infer_letrec_function_signatures(bindings);
                for binding in bindings {
                    if matches!(binding.value.kind, ExprKind::Lambda { .. }) {
                        let signature = function_signatures
                            .get(&binding.name)
                            .copied()
                            .unwrap_or_else(|| {
                                self.default_signature(&Formals {
                                    required: Vec::new(),
                                    rest: None,
                                })
                            });
                        scoped.insert(binding.name.clone(), BindingKind::Function(signature));
                    }
                }
                self.infer_expr_kind(body, &scoped, functions)
            }
            ExprKind::Guard {
                name,
                handler,
                body,
            } => {
                let mut scoped = env.clone();
                scoped.insert(name.clone(), BindingKind::Value(AbiValueKind::Word));
                combine_abi_kind(
                    self.infer_expr_kind(body, env, functions),
                    self.infer_expr_kind(handler, &scoped, functions),
                )
            }
            ExprKind::Delay(_) => AbiValueKind::Heap(HeapValueKind::Promise),
            ExprKind::Force(_) => AbiValueKind::Word,
            ExprKind::If {
                then_branch,
                else_branch,
                ..
            } => {
                let then_kind = self.infer_expr_kind(then_branch, env, functions);
                let else_kind = self.infer_expr_kind(else_branch, env, functions);
                if then_kind == else_kind {
                    then_kind
                } else {
                    AbiValueKind::Word
                }
            }
            ExprKind::Call { callee, .. } => self.infer_call_kind(callee, env, functions),
            ExprKind::Lambda { .. } => AbiValueKind::Word,
        }
    }

    fn infer_call_kind(
        &self,
        callee: &Expr,
        env: &HashMap<String, BindingKind>,
        functions: &HashMap<String, FunctionSignature>,
    ) -> AbiValueKind {
        match &callee.kind {
            ExprKind::Variable(name) => {
                if let Some(BindingKind::Function(signature)) = env.get(name) {
                    signature.return_kind
                } else if is_builtin(name) {
                    match name.as_str() {
                        "cons" => AbiValueKind::Heap(HeapValueKind::Pair),
                        "list" => AbiValueKind::Heap(HeapValueKind::Pair),
                        "vector" => AbiValueKind::Heap(HeapValueKind::Vector),
                        _ => AbiValueKind::Word,
                    }
                } else {
                    functions
                        .get(name)
                        .map(|signature| signature.return_kind)
                        .unwrap_or(AbiValueKind::Word)
                }
            }
            _ => AbiValueKind::Word,
        }
    }

    fn infer_param_kinds(&self, params: &[String], body: &Expr) -> Vec<AbiValueKind> {
        params
            .iter()
            .map(|param| self.infer_param_kind(param, body))
            .collect()
    }

    fn infer_param_kind(&self, param: &str, expr: &Expr) -> AbiValueKind {
        match &expr.kind {
            ExprKind::Unspecified
            | ExprKind::Variable(_)
            | ExprKind::Integer(_)
            | ExprKind::Boolean(_)
            | ExprKind::Char(_)
            | ExprKind::String(_) => AbiValueKind::Word,
            ExprKind::Set { value, .. } => self.infer_param_kind(param, value),
            ExprKind::Quote(_) => AbiValueKind::Word,
            ExprKind::Begin(exprs) => exprs.iter().fold(AbiValueKind::Word, |kind, expr| {
                combine_abi_kind(kind, self.infer_param_kind(param, expr))
            }),
            ExprKind::Let { bindings, body } | ExprKind::LetStar { bindings, body } => {
                let mut kind = AbiValueKind::Word;
                for binding in bindings {
                    if binding.name != param {
                        kind = combine_abi_kind(kind, self.infer_param_kind(param, &binding.value));
                    }
                }
                combine_abi_kind(kind, self.infer_param_kind(param, body))
            }
            ExprKind::LetRec { bindings, body } => {
                let mut kind = AbiValueKind::Word;
                for binding in bindings {
                    if binding.name != param {
                        kind = combine_abi_kind(kind, self.infer_param_kind(param, &binding.value));
                    }
                }
                combine_abi_kind(kind, self.infer_param_kind(param, body))
            }
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => combine_abi_kind(
                self.infer_param_kind(param, condition),
                combine_abi_kind(
                    self.infer_param_kind(param, then_branch),
                    self.infer_param_kind(param, else_branch),
                ),
            ),
            ExprKind::Guard { body, handler, .. } => combine_abi_kind(
                self.infer_param_kind(param, body),
                self.infer_param_kind(param, handler),
            ),
            ExprKind::Call { callee, args } => {
                if let ExprKind::Variable(name) = &callee.kind {
                    match name.as_str() {
                        "car" | "cdr" if args.first().is_some_and(|arg| matches!(&arg.kind, ExprKind::Variable(variable) if variable == param)) => {
                            return AbiValueKind::Heap(HeapValueKind::Pair);
                        }
                        "string-length" if args.first().is_some_and(|arg| matches!(&arg.kind, ExprKind::Variable(variable) if variable == param)) => {
                            return AbiValueKind::Heap(HeapValueKind::String);
                        }
                        "string-ref" if args.first().is_some_and(|arg| matches!(&arg.kind, ExprKind::Variable(variable) if variable == param)) => {
                            return AbiValueKind::Heap(HeapValueKind::String);
                        }
                        "vector-length" if args.first().is_some_and(|arg| matches!(&arg.kind, ExprKind::Variable(variable) if variable == param)) => {
                            return AbiValueKind::Heap(HeapValueKind::Vector);
                        }
                        "vector-ref" if args.first().is_some_and(|arg| matches!(&arg.kind, ExprKind::Variable(variable) if variable == param)) => {
                            return AbiValueKind::Heap(HeapValueKind::Vector);
                        }
                        "vector-set!" if args.first().is_some_and(|arg| matches!(&arg.kind, ExprKind::Variable(variable) if variable == param)) => {
                            return AbiValueKind::Heap(HeapValueKind::Vector);
                        }
                        "force" if args.first().is_some_and(|arg| matches!(&arg.kind, ExprKind::Variable(variable) if variable == param)) => {
                            return AbiValueKind::Heap(HeapValueKind::Promise);
                        }
                        _ => {}
                    }
                }
                args.iter()
                    .fold(self.infer_param_kind(param, callee), |kind, arg| {
                        combine_abi_kind(kind, self.infer_param_kind(param, arg))
                    })
            }
            ExprKind::Lambda { formals, body } => {
                if formals.required.iter().any(|candidate| candidate == param)
                    || formals.rest.as_deref() == Some(param)
                {
                    AbiValueKind::Word
                } else {
                    self.infer_param_kind(param, body)
                }
            }
            ExprKind::Delay(inner) | ExprKind::Force(inner) => self.infer_param_kind(param, inner),
        }
    }

    fn collect_captures(
        &self,
        env: &HashMap<String, CodegenValue<'ctx>>,
        formals: &Formals,
        body: &Expr,
    ) -> Result<Vec<(String, CaptureKind)>, CompileError> {
        let env_names = env
            .keys()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        let mut bound = formals.all_names();
        let mut names = std::collections::BTreeSet::new();
        self.collect_free_vars(body, &env_names, &mut bound, &mut names);

        let mut captures = Vec::with_capacity(names.len());
        for name in names {
            let value = env.get(&name).copied().ok_or_else(|| {
                CompileError::Codegen(format!(
                    "captured variable '{name}' is missing from the environment"
                ))
            })?;
            let kind = match value {
                CodegenValue::Word(_) | CodegenValue::RootedWord { .. } => {
                    CaptureKind::Value(AbiValueKind::Word)
                }
                CodegenValue::MutableBox { .. } => {
                    CaptureKind::Value(AbiValueKind::Heap(HeapValueKind::Box))
                }
                CodegenValue::HeapObject { kind, .. } => {
                    CaptureKind::Value(AbiValueKind::Heap(kind))
                }
                CodegenValue::Function(info) => CaptureKind::Function(info.signature),
                CodegenValue::Closure(info) => CaptureKind::Function(info.signature),
            };
            captures.push((name, kind));
        }
        Ok(captures)
    }

    fn collect_free_vars(
        &self,
        expr: &Expr,
        env_names: &std::collections::HashSet<String>,
        bound: &mut Vec<String>,
        out: &mut std::collections::BTreeSet<String>,
    ) {
        match &expr.kind {
            ExprKind::Unspecified
            | ExprKind::Integer(_)
            | ExprKind::Boolean(_)
            | ExprKind::Char(_)
            | ExprKind::String(_)
            | ExprKind::Quote(_) => {}
            ExprKind::Variable(name) => {
                if env_names.contains(name) && !bound.iter().any(|bound_name| bound_name == name) {
                    out.insert(name.clone());
                }
            }
            ExprKind::Set { name, value } => {
                if env_names.contains(name) && !bound.iter().any(|bound_name| bound_name == name) {
                    out.insert(name.clone());
                }
                self.collect_free_vars(value, env_names, bound, out);
            }
            ExprKind::Begin(exprs) => {
                for expr in exprs {
                    self.collect_free_vars(expr, env_names, bound, out);
                }
            }
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.collect_free_vars(condition, env_names, bound, out);
                self.collect_free_vars(then_branch, env_names, bound, out);
                self.collect_free_vars(else_branch, env_names, bound, out);
            }
            ExprKind::Call { callee, args } => {
                self.collect_free_vars(callee, env_names, bound, out);
                for arg in args {
                    self.collect_free_vars(arg, env_names, bound, out);
                }
            }
            ExprKind::Guard {
                name,
                handler,
                body,
            } => {
                self.collect_free_vars(body, env_names, bound, out);
                let start = bound.len();
                bound.push(name.clone());
                self.collect_free_vars(handler, env_names, bound, out);
                bound.truncate(start);
            }
            ExprKind::Delay(expr) | ExprKind::Force(expr) => {
                self.collect_free_vars(expr, env_names, bound, out);
            }
            ExprKind::Lambda {
                formals: params,
                body,
            } => {
                let start = bound.len();
                bound.extend(params.all_names());
                self.collect_free_vars(body, env_names, bound, out);
                bound.truncate(start);
            }
            ExprKind::Let { bindings, body } => {
                for binding in bindings {
                    self.collect_free_vars(&binding.value, env_names, bound, out);
                }
                let start = bound.len();
                bound.extend(bindings.iter().map(|binding| binding.name.clone()));
                self.collect_free_vars(body, env_names, bound, out);
                bound.truncate(start);
            }
            ExprKind::LetStar { bindings, body } => {
                let start = bound.len();
                for binding in bindings {
                    self.collect_free_vars(&binding.value, env_names, bound, out);
                    bound.push(binding.name.clone());
                }
                self.collect_free_vars(body, env_names, bound, out);
                bound.truncate(start);
            }
            ExprKind::LetRec { bindings, body } => {
                let start = bound.len();
                bound.extend(bindings.iter().map(|binding| binding.name.clone()));
                for binding in bindings {
                    self.collect_free_vars(&binding.value, env_names, bound, out);
                }
                self.collect_free_vars(body, env_names, bound, out);
                bound.truncate(start);
            }
        }
    }

    fn compile_closure_body(
        &mut self,
        function: FunctionValue<'ctx>,
        signature: FunctionSignature,
        formals: &Formals,
        captures: &[(String, CaptureKind)],
        body: &Expr,
    ) -> Result<(), CompileError> {
        if function.get_first_basic_block().is_some() {
            return Ok(());
        }

        let builder = self.context.create_builder();
        let entry = self.context.append_basic_block(function, "entry");
        let exception_block = self
            .context
            .append_basic_block(function, "exception.return");
        builder.position_at_end(entry);
        self.exception_targets.push(ExceptionTarget {
            block: exception_block,
        });

        let closure_env = function.get_first_param().ok_or_else(|| {
            CompileError::Codegen(format!(
                "missing closure environment parameter for function '{}'",
                function.get_name().to_str().unwrap_or("<lambda>")
            ))
        })?;

        let mut env = HashMap::new();
        let formal_names = formals.all_names();
        let mutated_names = self.collect_mutated_names_with_initial(body, formal_names.clone());
        let pair_mutated_names = self.collect_pair_mutated_names_with_initial(body, formal_names);
        let mut rooted_param_count = 0usize;
        for (index, (name, kind)) in captures.iter().enumerate() {
            let captured = self.load_closure_env_word(
                &builder,
                closure_env.into_pointer_value(),
                index,
                &format!("closure.capture.{index}"),
            )?;
            let value = match kind {
                CaptureKind::Value(AbiValueKind::Word) => CodegenValue::Word(captured),
                CaptureKind::Value(AbiValueKind::Heap(heap_kind))
                    if *heap_kind == HeapValueKind::Box =>
                {
                    CodegenValue::MutableBox {
                        ptr: builder
                            .build_int_to_ptr(
                                captured,
                                gc_ptr_type(self.context),
                                &format!("closure.capture.{index}.box"),
                            )
                            .map_err(|error| CompileError::Codegen(error.to_string()))?,
                    }
                }
                CaptureKind::Value(AbiValueKind::Heap(heap_kind)) => CodegenValue::HeapObject {
                    ptr: builder
                        .build_int_to_ptr(
                            captured,
                            gc_ptr_type(self.context),
                            &format!("closure.capture.{index}.ptr"),
                        )
                        .map_err(|error| CompileError::Codegen(error.to_string()))?,
                    kind: *heap_kind,
                },
                CaptureKind::Function(signature) => CodegenValue::Closure(ClosureInfo {
                    ptr: builder
                        .build_int_to_ptr(
                            captured,
                            gc_ptr_type(self.context),
                            &format!("closure.capture.{index}.closure"),
                        )
                        .map_err(|error| CompileError::Codegen(error.to_string()))?,
                    signature: *signature,
                }),
            };
            env.insert(name.clone(), value);
        }

        for (index, param_name) in formals.required.iter().enumerate() {
            let param = function.get_nth_param((index + 1) as u32).ok_or_else(|| {
                CompileError::Codegen(format!(
                    "missing parameter {index} for function '{}'",
                    function.get_name().to_str().unwrap_or("<lambda>")
                ))
            })?;
            let value = match signature
                .required_param_kinds
                .get(index)
                .copied()
                .unwrap_or(AbiValueKind::Word)
            {
                AbiValueKind::Word => CodegenValue::Word(param.into_int_value()),
                AbiValueKind::Heap(kind) => CodegenValue::HeapObject {
                    ptr: param.into_pointer_value(),
                    kind,
                },
            };
            let stored = if mutated_names.contains(param_name) {
                self.box_value(&builder, value, &format!("closure.param.{param_name}.box"))?
            } else if pair_mutated_names.contains(param_name) {
                rooted_param_count += 1;
                self.root_word(
                    &builder,
                    self.value_to_word(
                        &builder,
                        value,
                        &format!("closure.param.{param_name}.word"),
                    )?,
                    &format!("closure.param.{param_name}.root"),
                )?
            } else {
                value
            };
            env.insert(param_name.clone(), stored);
        }
        if let Some(rest_name) = &formals.rest {
            let index = formals.required.len() + 1;
            let param = function.get_nth_param(index as u32).ok_or_else(|| {
                CompileError::Codegen(format!(
                    "missing rest parameter for function '{}'",
                    function.get_name().to_str().unwrap_or("<lambda>")
                ))
            })?;
            let value = CodegenValue::Word(param.into_int_value());
            let stored = if mutated_names.contains(rest_name) {
                self.box_value(&builder, value, &format!("closure.param.{rest_name}.box"))?
            } else if pair_mutated_names.contains(rest_name) {
                rooted_param_count += 1;
                self.root_word(
                    &builder,
                    self.value_to_word(
                        &builder,
                        value,
                        &format!("closure.param.{rest_name}.word"),
                    )?,
                    &format!("closure.param.{rest_name}.root"),
                )?
            } else {
                value
            };
            env.insert(rest_name.clone(), stored);
        }

        self.compile_tail_expr(
            &builder,
            function,
            signature,
            &env,
            body,
            rooted_param_count,
        )?;
        self.emit_exception_return_block(
            exception_block,
            signature.return_kind,
            rooted_param_count,
        )?;
        self.exception_targets.pop();

        if function.verify(true) {
            Ok(())
        } else {
            Err(CompileError::Codegen(format!(
                "llvm verification failed for function '{}'",
                function.get_name().to_str().unwrap_or("<lambda>")
            )))
        }
    }

    fn compile_closure_scheme_wrapper(
        &mut self,
        wrapper: FunctionValue<'ctx>,
        function: FunctionValue<'ctx>,
        signature: FunctionSignature,
        formals: &Formals,
    ) -> Result<(), CompileError> {
        if wrapper.get_first_basic_block().is_some() {
            return Ok(());
        }

        let builder = self.context.create_builder();
        let entry = self.context.append_basic_block(wrapper, "entry");
        builder.position_at_end(entry);
        let closure_ptr = wrapper
            .get_first_param()
            .ok_or_else(|| {
                CompileError::Codegen("missing closure parameter for Scheme wrapper".into())
            })?
            .into_pointer_value();
        let args_list = wrapper
            .get_nth_param(1)
            .ok_or_else(|| {
                CompileError::Codegen("missing args parameter for Scheme wrapper".into())
            })?
            .into_int_value();

        if !signature.rest {
            self.assert_list_length(
                &builder,
                wrapper,
                args_list,
                signature.required_param_kinds.len(),
            )?;
        } else {
            self.assert_list_length_at_least(
                &builder,
                wrapper,
                args_list,
                signature.required_param_kinds.len(),
            )?;
        }

        let mut args = Vec::with_capacity(
            signature.required_param_kinds.len() + 1 + usize::from(signature.rest),
        );
        args.push(closure_ptr.into());
        for (index, kind) in signature.required_param_kinds.iter().enumerate() {
            let loaded =
                self.load_list_element(&builder, args_list, index, *kind, "closure.scheme.arg")?;
            args.push(self.convert_argument_value(
                &builder,
                loaded,
                *kind,
                "closure.scheme.arg",
            )?);
        }
        if formals.rest.is_some() {
            let rest = self.list_tail_word(
                &builder,
                args_list,
                signature.required_param_kinds.len(),
                "closure.scheme.rest",
            )?;
            args.push(rest.into());
        }

        let result = builder
            .build_call(function, &args, "closure.scheme.call")
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| {
                CompileError::Codegen("closure Scheme wrapper did not return a value".into())
            })?;
        let tail_pending = builder
            .build_call(
                self.runtime.rt_tail_pending,
                &[],
                "closure.scheme.tail_pending",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("tail pending did not return a value".into()))?
            .into_int_value();
        let tail_block = self.context.append_basic_block(wrapper, "tail.pending");
        let value_block = self.context.append_basic_block(wrapper, "tail.value");
        builder
            .build_conditional_branch(tail_pending, tail_block, value_block)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;

        builder.position_at_end(tail_block);
        let marker = builder
            .build_call(
                self.runtime.rt_tail_call_marker,
                &[],
                "closure.scheme.tail_marker",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CompileError::Codegen("tail marker did not return a value".into()))?
            .into_int_value();
        builder
            .build_return(Some(&marker))
            .map_err(|error| CompileError::Codegen(error.to_string()))?;

        builder.position_at_end(value_block);
        let wrapped = match signature.return_kind {
            AbiValueKind::Word => result.into_int_value(),
            AbiValueKind::Heap(_) => builder
                .build_ptr_to_int(
                    result.into_pointer_value(),
                    self.word_type(),
                    "closure.scheme.word",
                )
                .map_err(|error| CompileError::Codegen(error.to_string()))?,
        };
        builder
            .build_return(Some(&wrapped))
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        if wrapper.verify(true) {
            Ok(())
        } else {
            Err(CompileError::Codegen(format!(
                "llvm verification failed for closure Scheme wrapper '{}'",
                wrapper.get_name().to_str().unwrap_or("<wrapper>")
            )))
        }
    }

    fn compile_closure_allocation(
        &self,
        builder: &Builder<'ctx>,
        env: &HashMap<String, CodegenValue<'ctx>>,
        wrapper: FunctionValue<'ctx>,
        signature: FunctionSignature,
        captures: &[(String, CaptureKind)],
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let raw_ptr_type = self.context.ptr_type(inkwell::AddressSpace::default());
        let env_ptr = if captures.is_empty() {
            raw_ptr_type.const_null()
        } else {
            let len = self.word_type().const_int(captures.len() as u64, false);
            let values = builder
                .build_array_alloca(self.word_type(), len, "closure.env")
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            for (index, (name, _)) in captures.iter().enumerate() {
                let value = env.get(name).copied().ok_or_else(|| {
                    CompileError::Codegen(format!(
                        "captured variable '{name}' is missing from the environment"
                    ))
                })?;
                let word = self.value_to_word(builder, value, "closure capture")?;
                let slot = unsafe {
                    builder.build_gep(
                        self.word_type(),
                        values,
                        &[self.word_type().const_int(index as u64, false)],
                        &format!("closure.env.{index}"),
                    )
                }
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
                builder
                    .build_store(slot, word)
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
            }
            builder
                .build_pointer_cast(values, raw_ptr_type, "closure.env.raw")
                .map_err(|error| CompileError::Codegen(error.to_string()))?
        };

        let code_ptr = builder
            .build_ptr_to_int(
                wrapper.as_global_value().as_pointer_value(),
                self.word_type(),
                "closure.code.ptr",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let closure = builder
            .build_call(
                self.runtime.alloc_closure_gc,
                &[
                    code_ptr.into(),
                    env_ptr.into(),
                    self.word_type()
                        .const_int(captures.len() as u64, false)
                        .into(),
                ],
                "closure.alloc",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| {
                CompileError::Codegen("closure allocation did not return a value".into())
            })?
            .into_pointer_value();

        Ok(CodegenValue::Closure(ClosureInfo {
            ptr: closure,
            signature,
        }))
    }

    fn allocate_placeholder_closure(
        &self,
        builder: &Builder<'ctx>,
        wrapper: FunctionValue<'ctx>,
        signature: FunctionSignature,
        env_len: usize,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        let raw_ptr_type = self.context.ptr_type(inkwell::AddressSpace::default());
        let env_ptr = if env_len == 0 {
            raw_ptr_type.const_null()
        } else {
            let len = self.word_type().const_int(env_len as u64, false);
            let values = builder
                .build_array_alloca(self.word_type(), len, "letrec.closure.env")
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
            let unspecified = self
                .word_type()
                .const_int(Value::unspecified().bits() as u64, false);
            for index in 0..env_len {
                let slot = unsafe {
                    builder.build_gep(
                        self.word_type(),
                        values,
                        &[self.word_type().const_int(index as u64, false)],
                        &format!("letrec.closure.env.{index}"),
                    )
                }
                .map_err(|error| CompileError::Codegen(error.to_string()))?;
                builder
                    .build_store(slot, unspecified)
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
            }
            builder
                .build_pointer_cast(values, raw_ptr_type, "letrec.closure.env.raw")
                .map_err(|error| CompileError::Codegen(error.to_string()))?
        };
        let code_ptr = builder
            .build_ptr_to_int(
                wrapper.as_global_value().as_pointer_value(),
                self.word_type(),
                "letrec.closure.code.ptr",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let closure = builder
            .build_call(
                self.runtime.alloc_closure_gc,
                &[
                    code_ptr.into(),
                    env_ptr.into(),
                    self.word_type().const_int(env_len as u64, false).into(),
                ],
                "letrec.closure.alloc",
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| {
                CompileError::Codegen("letrec closure allocation did not return a value".into())
            })?
            .into_pointer_value();
        Ok(CodegenValue::Closure(ClosureInfo {
            ptr: closure,
            signature,
        }))
    }

    fn collect_program_mutations(&self, program: &Program) -> std::collections::HashSet<String> {
        let mut mutated = std::collections::HashSet::new();
        let mut bound = Vec::new();
        for item in &program.items {
            match item {
                TopLevel::Definition { name, value } => {
                    bound.push(name.clone());
                    self.collect_mutated_in_expr(value, &mut bound, &mut mutated);
                }
                TopLevel::Procedure(procedure) => {
                    bound.push(procedure.name.clone());
                    self.collect_mutated_in_expr(&procedure.body, &mut bound, &mut mutated);
                }
                TopLevel::Expression(expr) => {
                    self.collect_mutated_in_expr(expr, &mut bound, &mut mutated)
                }
            }
        }
        mutated
    }

    fn collect_program_pair_mutations(
        &self,
        program: &Program,
    ) -> std::collections::HashSet<String> {
        let mut pair_mutated = std::collections::HashSet::new();
        let mut bound = Vec::new();
        for item in &program.items {
            match item {
                TopLevel::Definition { name, value } => {
                    bound.push(name.clone());
                    self.collect_pair_mutated_in_expr(value, &mut bound, &mut pair_mutated);
                }
                TopLevel::Procedure(procedure) => {
                    bound.push(procedure.name.clone());
                    self.collect_pair_mutated_in_expr(
                        &procedure.body,
                        &mut bound,
                        &mut pair_mutated,
                    );
                }
                TopLevel::Expression(expr) => {
                    self.collect_pair_mutated_in_expr(expr, &mut bound, &mut pair_mutated);
                }
            }
        }
        pair_mutated
    }

    fn collect_mutated_names_with_initial(
        &self,
        expr: &Expr,
        initial: impl IntoIterator<Item = String>,
    ) -> std::collections::HashSet<String> {
        let mut mutated = std::collections::HashSet::new();
        let mut bound = initial.into_iter().collect::<Vec<_>>();
        self.collect_mutated_in_expr(expr, &mut bound, &mut mutated);
        mutated
    }

    fn collect_pair_mutated_names_with_initial(
        &self,
        expr: &Expr,
        initial: impl IntoIterator<Item = String>,
    ) -> std::collections::HashSet<String> {
        let mut pair_mutated = std::collections::HashSet::new();
        let mut bound = initial.into_iter().collect::<Vec<_>>();
        self.collect_pair_mutated_in_expr(expr, &mut bound, &mut pair_mutated);
        pair_mutated
    }

    fn collect_mutated_in_expr(
        &self,
        expr: &Expr,
        bound: &mut Vec<String>,
        out: &mut std::collections::HashSet<String>,
    ) {
        match &expr.kind {
            ExprKind::Unspecified
            | ExprKind::Integer(_)
            | ExprKind::Boolean(_)
            | ExprKind::Char(_)
            | ExprKind::String(_)
            | ExprKind::Variable(_)
            | ExprKind::Quote(_) => {}
            ExprKind::Set { name, value } => {
                if bound.iter().any(|bound_name| bound_name == name) {
                    out.insert(name.clone());
                }
                self.collect_mutated_in_expr(value, bound, out);
            }
            ExprKind::Call { callee, args } => {
                self.collect_mutated_in_expr(callee, bound, out);
                for arg in args {
                    self.collect_mutated_in_expr(arg, bound, out);
                }
            }
            ExprKind::Begin(exprs) => {
                for expr in exprs {
                    self.collect_mutated_in_expr(expr, bound, out);
                }
            }
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.collect_mutated_in_expr(condition, bound, out);
                self.collect_mutated_in_expr(then_branch, bound, out);
                self.collect_mutated_in_expr(else_branch, bound, out);
            }
            ExprKind::Guard {
                name,
                handler,
                body,
            } => {
                self.collect_mutated_in_expr(body, bound, out);
                let start = bound.len();
                bound.push(name.clone());
                self.collect_mutated_in_expr(handler, bound, out);
                bound.truncate(start);
            }
            ExprKind::Delay(expr) | ExprKind::Force(expr) => {
                self.collect_mutated_in_expr(expr, bound, out);
            }
            ExprKind::Lambda {
                formals: params,
                body,
            } => {
                let start = bound.len();
                bound.extend(params.all_names());
                self.collect_mutated_in_expr(body, bound, out);
                bound.truncate(start);
            }
            ExprKind::Let { bindings, body } => {
                for binding in bindings {
                    self.collect_mutated_in_expr(&binding.value, bound, out);
                }
                let start = bound.len();
                bound.extend(bindings.iter().map(|binding| binding.name.clone()));
                self.collect_mutated_in_expr(body, bound, out);
                bound.truncate(start);
            }
            ExprKind::LetStar { bindings, body } => {
                let start = bound.len();
                for binding in bindings {
                    self.collect_mutated_in_expr(&binding.value, bound, out);
                    bound.push(binding.name.clone());
                }
                self.collect_mutated_in_expr(body, bound, out);
                bound.truncate(start);
            }
            ExprKind::LetRec { bindings, body } => {
                let start = bound.len();
                bound.extend(bindings.iter().map(|binding| binding.name.clone()));
                for binding in bindings {
                    self.collect_mutated_in_expr(&binding.value, bound, out);
                }
                self.collect_mutated_in_expr(body, bound, out);
                bound.truncate(start);
            }
        }
    }

    fn collect_pair_mutated_in_expr(
        &self,
        expr: &Expr,
        bound: &mut Vec<String>,
        out: &mut std::collections::HashSet<String>,
    ) {
        match &expr.kind {
            ExprKind::Unspecified
            | ExprKind::Integer(_)
            | ExprKind::Boolean(_)
            | ExprKind::Char(_)
            | ExprKind::String(_)
            | ExprKind::Variable(_)
            | ExprKind::Quote(_) => {}
            ExprKind::Set { value, .. } => {
                self.collect_pair_mutated_in_expr(value, bound, out);
            }
            ExprKind::Call { callee, args } => {
                if let ExprKind::Variable(name) = &callee.kind
                    && matches!(name.as_str(), "set-car!" | "set-cdr!")
                    && let Some(Expr {
                        kind: ExprKind::Variable(target),
                    }) = args.first()
                    && bound.iter().any(|bound_name| bound_name == target)
                {
                    out.insert(target.clone());
                }
                self.collect_pair_mutated_in_expr(callee, bound, out);
                for arg in args {
                    self.collect_pair_mutated_in_expr(arg, bound, out);
                }
            }
            ExprKind::Begin(exprs) => {
                for expr in exprs {
                    self.collect_pair_mutated_in_expr(expr, bound, out);
                }
            }
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.collect_pair_mutated_in_expr(condition, bound, out);
                self.collect_pair_mutated_in_expr(then_branch, bound, out);
                self.collect_pair_mutated_in_expr(else_branch, bound, out);
            }
            ExprKind::Guard {
                name,
                handler,
                body,
            } => {
                self.collect_pair_mutated_in_expr(body, bound, out);
                let start = bound.len();
                bound.push(name.clone());
                self.collect_pair_mutated_in_expr(handler, bound, out);
                bound.truncate(start);
            }
            ExprKind::Delay(expr) | ExprKind::Force(expr) => {
                self.collect_pair_mutated_in_expr(expr, bound, out);
            }
            ExprKind::Lambda {
                formals: params,
                body,
            } => {
                let start = bound.len();
                bound.extend(params.all_names());
                self.collect_pair_mutated_in_expr(body, bound, out);
                bound.truncate(start);
            }
            ExprKind::Let { bindings, body } => {
                for binding in bindings {
                    self.collect_pair_mutated_in_expr(&binding.value, bound, out);
                }
                let start = bound.len();
                bound.extend(bindings.iter().map(|binding| binding.name.clone()));
                self.collect_pair_mutated_in_expr(body, bound, out);
                bound.truncate(start);
            }
            ExprKind::LetStar { bindings, body } => {
                let start = bound.len();
                for binding in bindings {
                    self.collect_pair_mutated_in_expr(&binding.value, bound, out);
                    bound.push(binding.name.clone());
                }
                self.collect_pair_mutated_in_expr(body, bound, out);
                bound.truncate(start);
            }
            ExprKind::LetRec { bindings, body } => {
                let start = bound.len();
                bound.extend(bindings.iter().map(|binding| binding.name.clone()));
                for binding in bindings {
                    self.collect_pair_mutated_in_expr(&binding.value, bound, out);
                }
                self.collect_pair_mutated_in_expr(body, bound, out);
                bound.truncate(start);
            }
        }
    }

    fn const_fixnum(&self, value: i64) -> IntValue<'ctx> {
        self.word_type()
            .const_int(((value << FIXNUM_SHIFT) as u64) | FIXNUM_TAG as u64, true)
    }

    fn const_fixnum_checked(&self, value: i64) -> Result<IntValue<'ctx>, CompileError> {
        let encoded = Value::encode_fixnum(value).ok_or_else(|| {
            CompileError::Codegen(format!(
                "integer literal {value} does not fit in the tagged fixnum representation"
            ))
        })?;
        Ok(self.word_type().const_int(encoded.bits() as u64, false))
    }

    fn const_bool(&self, value: bool) -> IntValue<'ctx> {
        let bits = if value { BOOL_TRUE } else { BOOL_FALSE };
        self.word_type().const_int(bits as u64, false)
    }

    fn decode_fixnum(
        &self,
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        name: &str,
    ) -> Result<IntValue<'ctx>, CompileError> {
        builder
            .build_right_shift(
                value,
                self.word_type().const_int(FIXNUM_SHIFT as u64, false),
                true,
                name,
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))
    }

    fn ensure_fixnum(
        &mut self,
        builder: &Builder<'ctx>,
        current_function: FunctionValue<'ctx>,
        value: IntValue<'ctx>,
        name: &str,
    ) -> Result<IntValue<'ctx>, CompileError> {
        let is_fixnum = builder
            .build_int_compare(
                IntPredicate::EQ,
                builder
                    .build_and(
                        value,
                        self.word_type().const_int(FIXNUM_TAG as u64, false),
                        &format!("{name}.mask"),
                    )
                    .map_err(|error| CompileError::Codegen(error.to_string()))?,
                self.word_type().const_int(FIXNUM_TAG as u64, false),
                &format!("{name}.is_fixnum"),
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;

        let ok_block = self
            .context
            .append_basic_block(current_function, &format!("{name}.ok"));
        let trap_block = self
            .context
            .append_basic_block(current_function, &format!("{name}.trap"));

        builder
            .build_conditional_branch(is_fixnum, ok_block, trap_block)
            .map_err(|error| CompileError::Codegen(error.to_string()))?;

        builder.position_at_end(trap_block);
        builder
            .build_call(self.trap_intrinsic(), &[], "")
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        builder
            .build_unreachable()
            .map_err(|error| CompileError::Codegen(error.to_string()))?;

        builder.position_at_end(ok_block);
        Ok(value)
    }

    fn encode_fixnum_value(
        &self,
        builder: &Builder<'ctx>,
        value: IntValue<'ctx>,
        name: &str,
    ) -> Result<IntValue<'ctx>, CompileError> {
        let shifted = builder
            .build_left_shift(
                value,
                self.word_type().const_int(FIXNUM_SHIFT as u64, false),
                &format!("{name}.shift"),
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        builder
            .build_or(
                shifted,
                self.word_type().const_int(FIXNUM_TAG as u64, false),
                name,
            )
            .map_err(|error| CompileError::Codegen(error.to_string()))
    }

    fn expect_word(
        &self,
        value: CodegenValue<'ctx>,
        context: &str,
    ) -> Result<IntValue<'ctx>, CompileError> {
        match value {
            CodegenValue::Word(value) => Ok(value),
            CodegenValue::RootedWord { .. } => Err(CompileError::Codegen(format!(
                "{context} expected a Scheme word, but a rooted stack value was produced"
            ))),
            CodegenValue::MutableBox { .. } => Err(CompileError::Codegen(format!(
                "{context} expected a Scheme word, but a mutable binding cell was produced"
            ))),
            CodegenValue::HeapObject { kind, .. } => Err(CompileError::Codegen(format!(
                "{context} expected a Scheme word, but a {} heap reference was produced",
                heap_kind_name(kind)
            ))),
            CodegenValue::Function(_) | CodegenValue::Closure(_) => Err(CompileError::Codegen(
                format!("{context} expected a Scheme word, but a function was produced"),
            )),
        }
    }

    fn value_to_word(
        &self,
        builder: &Builder<'ctx>,
        value: CodegenValue<'ctx>,
        context: &str,
    ) -> Result<IntValue<'ctx>, CompileError> {
        match value {
            CodegenValue::Word(value) => Ok(value),
            CodegenValue::RootedWord { slot } => self.load_rooted_word(builder, slot, context),
            CodegenValue::MutableBox { ptr } => builder
                .build_ptr_to_int(ptr, self.word_type(), context)
                .map_err(|error| CompileError::Codegen(error.to_string())),
            CodegenValue::HeapObject { ptr, .. } => builder
                .build_ptr_to_int(ptr, self.word_type(), context)
                .map_err(|error| CompileError::Codegen(error.to_string())),
            CodegenValue::Closure(info) => builder
                .build_ptr_to_int(info.ptr, self.word_type(), context)
                .map_err(|error| CompileError::Codegen(error.to_string())),
            CodegenValue::Function(info) => {
                let closure =
                    self.allocate_placeholder_closure(builder, info.wrapper, info.signature, 0)?;
                let CodegenValue::Closure(closure) = closure else {
                    unreachable!()
                };
                builder
                    .build_ptr_to_int(closure.ptr, self.word_type(), context)
                    .map_err(|error| CompileError::Codegen(error.to_string()))
            }
        }
    }

    fn expect_heap_object(
        &self,
        value: CodegenValue<'ctx>,
        expected_kind: HeapValueKind,
        context: &str,
    ) -> Result<PointerValue<'ctx>, CompileError> {
        match value {
            CodegenValue::HeapObject { ptr, kind } if kind == expected_kind => Ok(ptr),
            CodegenValue::HeapObject { kind, .. } => Err(CompileError::Codegen(format!(
                "{context} expected a {} heap reference, but a {} heap reference was produced",
                heap_kind_name(expected_kind),
                heap_kind_name(kind)
            ))),
            CodegenValue::Word(_) | CodegenValue::RootedWord { .. } => {
                Err(CompileError::Codegen(format!(
                    "{context} expected a {} heap reference, but a Scheme word was produced",
                    heap_kind_name(expected_kind)
                )))
            }
            CodegenValue::MutableBox { .. } => Err(CompileError::Codegen(format!(
                "{context} expected a {} heap reference, but a mutable binding cell was produced",
                heap_kind_name(expected_kind)
            ))),
            CodegenValue::Function(_) | CodegenValue::Closure(_) => {
                Err(CompileError::Codegen(format!(
                    "{context} expected a {} heap reference, but a function was produced",
                    heap_kind_name(expected_kind)
                )))
            }
        }
    }

    fn merge_branch_values(
        &self,
        builder: &Builder<'ctx>,
        then_value: CodegenValue<'ctx>,
        then_block: inkwell::basic_block::BasicBlock<'ctx>,
        else_value: CodegenValue<'ctx>,
        else_block: inkwell::basic_block::BasicBlock<'ctx>,
        name: &str,
    ) -> Result<CodegenValue<'ctx>, CompileError> {
        match (then_value, else_value) {
            (CodegenValue::Word(then_word), CodegenValue::Word(else_word)) => {
                let phi = builder
                    .build_phi(self.word_type(), name)
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
                phi.add_incoming(&[(&then_word, then_block), (&else_word, else_block)]);
                Ok(CodegenValue::Word(phi.as_basic_value().into_int_value()))
            }
            (CodegenValue::RootedWord { .. }, _) | (_, CodegenValue::RootedWord { .. }) => {
                Err(CompileError::Codegen(
                    "if branches cannot currently merge rooted stack values".into(),
                ))
            }
            (
                CodegenValue::HeapObject {
                    ptr: then_ptr,
                    kind: then_kind,
                },
                CodegenValue::HeapObject {
                    ptr: else_ptr,
                    kind: else_kind,
                },
            ) if then_kind == else_kind => {
                let phi = builder
                    .build_phi(then_ptr.get_type(), name)
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
                phi.add_incoming(&[(&then_ptr, then_block), (&else_ptr, else_block)]);
                Ok(CodegenValue::HeapObject {
                    ptr: phi.as_basic_value().into_pointer_value(),
                    kind: then_kind,
                })
            }
            (
                CodegenValue::MutableBox { ptr: then_ptr },
                CodegenValue::MutableBox { ptr: else_ptr },
            ) => {
                let phi = builder
                    .build_phi(then_ptr.get_type(), name)
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
                phi.add_incoming(&[(&then_ptr, then_block), (&else_ptr, else_block)]);
                Ok(CodegenValue::MutableBox {
                    ptr: phi.as_basic_value().into_pointer_value(),
                })
            }
            (CodegenValue::Function(_), _) | (_, CodegenValue::Function(_)) => Err(
                CompileError::Codegen("if branches cannot currently merge function values".into()),
            ),
            (CodegenValue::Closure(then_info), CodegenValue::Closure(else_info))
                if then_info.signature == else_info.signature =>
            {
                let phi = builder
                    .build_phi(then_info.ptr.get_type(), name)
                    .map_err(|error| CompileError::Codegen(error.to_string()))?;
                phi.add_incoming(&[(&then_info.ptr, then_block), (&else_info.ptr, else_block)]);
                Ok(CodegenValue::Closure(ClosureInfo {
                    ptr: phi.as_basic_value().into_pointer_value(),
                    signature: then_info.signature,
                }))
            }
            (CodegenValue::Closure(_), _) | (_, CodegenValue::Closure(_)) => Err(
                CompileError::Codegen("if branches must produce the same kind of value".into()),
            ),
            _ => Err(CompileError::Codegen(
                "if branches must produce the same kind of value".into(),
            )),
        }
    }

    fn compare_codegen_values(
        &self,
        builder: &Builder<'ctx>,
        left: CodegenValue<'ctx>,
        right: CodegenValue<'ctx>,
        name: &str,
    ) -> Result<IntValue<'ctx>, CompileError> {
        match (left, right) {
            (CodegenValue::Word(lhs), CodegenValue::Word(rhs)) => builder
                .build_int_compare(IntPredicate::EQ, lhs, rhs, name)
                .map_err(|error| CompileError::Codegen(error.to_string())),
            (CodegenValue::RootedWord { slot: lhs }, CodegenValue::RootedWord { slot: rhs }) => {
                let lhs_word = self.load_rooted_word(builder, lhs, &format!("{name}.lhs.word"))?;
                let rhs_word = self.load_rooted_word(builder, rhs, &format!("{name}.rhs.word"))?;
                builder
                    .build_int_compare(IntPredicate::EQ, lhs_word, rhs_word, name)
                    .map_err(|error| CompileError::Codegen(error.to_string()))
            }
            (
                CodegenValue::HeapObject {
                    ptr: lhs,
                    kind: lhs_kind,
                },
                CodegenValue::HeapObject {
                    ptr: rhs,
                    kind: rhs_kind,
                },
            ) => {
                if lhs_kind != rhs_kind {
                    return Ok(self.context.bool_type().const_zero());
                }
                self.compare_gc_pointers(builder, lhs, rhs, name)
            }
            (CodegenValue::Closure(lhs), CodegenValue::Closure(rhs)) => {
                self.compare_gc_pointers(builder, lhs.ptr, rhs.ptr, name)
            }
            (CodegenValue::MutableBox { ptr: lhs }, CodegenValue::MutableBox { ptr: rhs }) => {
                self.compare_gc_pointers(builder, lhs, rhs, name)
            }
            (CodegenValue::RootedWord { .. }, _) | (_, CodegenValue::RootedWord { .. }) => {
                Ok(self.context.bool_type().const_zero())
            }
            _ => Ok(self.context.bool_type().const_zero()),
        }
    }

    fn compare_gc_pointers(
        &self,
        builder: &Builder<'ctx>,
        left: PointerValue<'ctx>,
        right: PointerValue<'ctx>,
        name: &str,
    ) -> Result<IntValue<'ctx>, CompileError> {
        let left_int = builder
            .build_ptr_to_int(left, self.word_type(), &format!("{name}.lhs"))
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        let right_int = builder
            .build_ptr_to_int(right, self.word_type(), &format!("{name}.rhs"))
            .map_err(|error| CompileError::Codegen(error.to_string()))?;
        builder
            .build_int_compare(IntPredicate::EQ, left_int, right_int, name)
            .map_err(|error| CompileError::Codegen(error.to_string()))
    }

    fn infer_quote_kind(&self, datum: &Datum) -> AbiValueKind {
        match datum {
            Datum::Integer(_) | Datum::Boolean(_) | Datum::Char(_) => AbiValueKind::Word,
            Datum::String(_) => AbiValueKind::Heap(HeapValueKind::String),
            Datum::Symbol(_) => AbiValueKind::Heap(HeapValueKind::Symbol),
            Datum::List { items, tail } if items.is_empty() && tail.is_none() => AbiValueKind::Word,
            Datum::List { .. } => AbiValueKind::Heap(HeapValueKind::Pair),
        }
    }

    fn trap_intrinsic(&self) -> FunctionValue<'ctx> {
        self.module.get_function("llvm.trap").unwrap_or_else(|| {
            self.module.add_function(
                "llvm.trap",
                self.context.void_type().fn_type(&[], false),
                None,
            )
        })
    }
}

fn is_builtin(name: &str) -> bool {
    builtin_names().contains(&name)
}

fn builtin_names() -> &'static [&'static str] {
    &[
        "+",
        "-",
        "*",
        "/",
        "=",
        "<",
        ">",
        "<=",
        ">=",
        "not",
        "boolean?",
        "zero?",
        "char?",
        "char=?",
        "char<?",
        "char>?",
        "char<=?",
        "char>=?",
        "char->integer",
        "integer->char",
        "symbol?",
        "symbol->string",
        "string->symbol",
        "procedure?",
        "values",
        "call-with-values",
        "raise",
        "error",
        "apply",
        "eq?",
        "eqv?",
        "equal?",
        "list",
        "map",
        "for-each",
        "append",
        "memq",
        "memv",
        "member",
        "assq",
        "assv",
        "assoc",
        "list-copy",
        "reverse",
        "cons",
        "car",
        "cdr",
        "set-car!",
        "set-cdr!",
        "pair?",
        "list?",
        "length",
        "list-tail",
        "list-ref",
        "null?",
        "string?",
        "string-length",
        "string-ref",
        "display",
        "write",
        "newline",
        "gc-stress",
        "vector",
        "vector?",
        "vector-length",
        "vector-ref",
        "vector-set!",
    ]
}

fn builtin_procedure_names() -> &'static [&'static str] {
    &[
        "+",
        "-",
        "*",
        "/",
        "=",
        "<",
        ">",
        "<=",
        ">=",
        "not",
        "boolean?",
        "zero?",
        "char?",
        "char=?",
        "char<?",
        "char>?",
        "char<=?",
        "char>=?",
        "char->integer",
        "integer->char",
        "symbol?",
        "symbol->string",
        "string->symbol",
        "procedure?",
        "eq?",
        "eqv?",
        "equal?",
        "list",
        "append",
        "memq",
        "memv",
        "member",
        "assq",
        "assv",
        "assoc",
        "list-copy",
        "reverse",
        "cons",
        "car",
        "cdr",
        "set-car!",
        "set-cdr!",
        "pair?",
        "list?",
        "length",
        "list-tail",
        "list-ref",
        "null?",
        "string?",
        "string-length",
        "string-ref",
        "display",
        "write",
        "newline",
        "gc-stress",
        "vector",
        "vector?",
        "vector-length",
        "vector-ref",
        "vector-set!",
        "raise",
        "error",
    ]
}

fn builtin_wrapper_signature(name: &str) -> FunctionSignature {
    let (required, rest) = match name {
        "+" | "*" | "list" | "append" | "vector" => (0, true),
        "-" => (1, true),
        "/" => (2, true),
        "=" | "<" | ">" | "<=" | ">=" => (2, true),
        "map" | "for-each" => (2, true),
        "not" | "boolean?" | "zero?" | "char?" | "char->integer" | "integer->char" | "symbol?"
        | "symbol->string" | "string->symbol" | "procedure?" | "list-copy" | "reverse" | "car"
        | "cdr" | "pair?" | "list?" | "length" | "null?" | "string?" | "string-length"
        | "display" | "write" | "gc-stress" | "vector?" | "vector-length" => (1, false),
        "newline" => (0, false),
        "raise" => (1, false),
        "error" => (1, true),
        "apply" => (1, true),
        "char=?" | "char<?" | "char>?" | "char<=?" | "char>=?" => (2, true),
        "eq?" | "eqv?" | "equal?" | "cons" | "set-car!" | "set-cdr!" | "list-tail" | "list-ref"
        | "string-ref" | "vector-ref" | "memq" | "memv" | "member" | "assq" | "assv" | "assoc" => {
            (2, false)
        }
        "vector-set!" => (3, false),
        _ => unreachable!(),
    };
    let param_kinds = vec![AbiValueKind::Word; required].into_boxed_slice();
    FunctionSignature {
        return_kind: AbiValueKind::Word,
        required_param_kinds: Box::leak(param_kinds),
        rest,
    }
}

fn builtin_procedure_id(name: &str) -> u16 {
    builtin_procedure_names()
        .iter()
        .position(|candidate| *candidate == name)
        .map(|index| index as u16)
        .unwrap_or_else(|| panic!("missing builtin procedure id for '{name}'"))
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

fn heap_kind_name(kind: HeapValueKind) -> &'static str {
    match kind {
        HeapValueKind::Pair => "pair",
        HeapValueKind::String => "string",
        HeapValueKind::Symbol => "symbol",
        HeapValueKind::Vector => "vector",
        HeapValueKind::Box => "box",
        HeapValueKind::Promise => "promise",
    }
}

fn combine_abi_kind(left: AbiValueKind, right: AbiValueKind) -> AbiValueKind {
    match (left, right) {
        (AbiValueKind::Word, AbiValueKind::Word) => AbiValueKind::Word,
        (AbiValueKind::Word, heap @ AbiValueKind::Heap(_))
        | (heap @ AbiValueKind::Heap(_), AbiValueKind::Word) => heap,
        (AbiValueKind::Heap(left_kind), AbiValueKind::Heap(right_kind))
            if left_kind == right_kind =>
        {
            AbiValueKind::Heap(left_kind)
        }
        _ => AbiValueKind::Word,
    }
}
