use super::llvm::CompiledModule;
use crate::error::CompileError;
use inkwell::AddressSpace;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::types::PointerType;
use inkwell::values::FunctionValue;

pub const GC_STRATEGY_NAME: &str = "coreclr";
pub const GC_HEAP_ADDRSPACE: u16 = 1;

pub fn heap_address_space() -> AddressSpace {
    AddressSpace::from(GC_HEAP_ADDRSPACE)
}

pub fn gc_ptr_type<'ctx>(context: &'ctx Context) -> PointerType<'ctx> {
    context.ptr_type(heap_address_space())
}

pub fn attach_gc_strategy(function: FunctionValue<'_>) {
    function.set_gc(GC_STRATEGY_NAME);
}

pub fn compile_pre_statepoint_example(module_name: &str) -> Result<CompiledModule, CompileError> {
    let context = Context::create();
    let module = context.create_module(module_name);
    let builder = context.create_builder();
    let void_type = context.void_type();
    let gc_ptr = gc_ptr_type(&context);
    let word_type = context.i64_type();
    let kind_type = context.i16_type();

    let runtime_poll = module.get_function("gc_safepoint_poll").unwrap_or_else(|| {
        module.add_function("gc_safepoint_poll", void_type.fn_type(&[], false), None)
    });
    module.add_function("rt_gc_poll", void_type.fn_type(&[], false), None);
    let alloc_slow_raw = module.add_function(
        "rt_alloc_slow",
        context
            .ptr_type(AddressSpace::default())
            .fn_type(&[word_type.into(), word_type.into(), kind_type.into()], false),
        None,
    );
    let alloc_slow = build_alloc_slow_wrapper(&context, &module, &builder, alloc_slow_raw)?;
    let poll_wrapper =
        module.add_function("gc.safepoint_poll", void_type.fn_type(&[], false), None);
    build_poll_wrapper(&context, &builder, poll_wrapper, runtime_poll)?;

    let foo = module.add_function("foo", void_type.fn_type(&[gc_ptr.into()], false), None);
    build_foo(&context, &builder, foo)?;

    let test = module.add_function("test", gc_ptr.fn_type(&[gc_ptr.into()], false), None);
    attach_gc_strategy(test);
    build_test(&context, &builder, test, foo, alloc_slow)?;

    if test.verify(true) && foo.verify(true) && poll_wrapper.verify(true) {
        Ok(CompiledModule {
            module_name: module_name.to_string(),
            llvm_ir: module.print_to_string().to_string(),
        })
    } else {
        Err(CompileError::Codegen(
            "failed to verify pre-statepoint example module".into(),
        ))
    }
}

pub fn statepoint_ir_example() -> &'static str {
    r#"define ptr addrspace(1) @test(ptr addrspace(1) %obj) gc "coreclr" {
entry:
  %safepoint = call token (i64, i32, ptr, i32, i32, ...) @llvm.experimental.gc.statepoint.p0f_isVoidf(i64 0, i32 0, ptr elementtype(void ()) @foo, i32 0, i32 0, i32 0, i32 0) ["gc-live"(ptr addrspace(1) %obj)]
  %obj.relocated = call ptr addrspace(1) @llvm.experimental.gc.relocate.p1(token %safepoint, i32 0, i32 0)
  ret ptr addrspace(1) %obj.relocated
}

define void @foo() {
entry:
  ret void
}

declare token @llvm.experimental.gc.statepoint.p0f_isVoidf(i64 immarg, i32 immarg, ptr elementtype(void ()), i32 immarg, i32 immarg, ...)
declare ptr addrspace(1) @llvm.experimental.gc.relocate.p1(token, i32 immarg, i32 immarg)
"#
}

fn build_poll_wrapper<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    poll_wrapper: FunctionValue<'ctx>,
    runtime_poll: FunctionValue<'ctx>,
) -> Result<(), CompileError> {
    let entry = context.append_basic_block(poll_wrapper, "entry");
    builder.position_at_end(entry);
    builder
        .build_direct_call(runtime_poll, &[], "")
        .map_err(|error| CompileError::Codegen(error.to_string()))?;
    builder
        .build_return(None)
        .map_err(|error| CompileError::Codegen(error.to_string()))?;
    Ok(())
}

fn build_foo<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    foo: FunctionValue<'ctx>,
) -> Result<(), CompileError> {
    let entry = context.append_basic_block(foo, "entry");
    builder.position_at_end(entry);
    builder
        .build_return(None)
        .map_err(|error| CompileError::Codegen(error.to_string()))?;
    Ok(())
}

fn build_alloc_slow_wrapper<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    builder: &Builder<'ctx>,
    alloc_slow_raw: FunctionValue<'ctx>,
) -> Result<FunctionValue<'ctx>, CompileError> {
    let gc_ptr = gc_ptr_type(context);
    let word_type = context.i64_type();
    let kind_type = context.i16_type();
    let wrapper = module.add_function(
        "rt_alloc_slow_as1",
        gc_ptr.fn_type(&[word_type.into(), word_type.into(), kind_type.into()], false),
        None,
    );
    let entry = context.append_basic_block(wrapper, "entry");
    builder.position_at_end(entry);
    let size = wrapper.get_nth_param(0).unwrap().into_int_value();
    let align = wrapper.get_nth_param(1).unwrap().into_int_value();
    let kind = wrapper.get_nth_param(2).unwrap().into_int_value();
    let raw = builder
        .build_direct_call(
            alloc_slow_raw,
            &[size.into(), align.into(), kind.into()],
            "raw_alloc",
        )
        .map_err(|error| CompileError::Codegen(error.to_string()))?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| CompileError::Codegen("rt_alloc_slow must return a pointer".into()))?
        .into_pointer_value();
    let cast = builder
        .build_address_space_cast(raw, gc_ptr, "gc_alloc")
        .map_err(|error| CompileError::Codegen(error.to_string()))?;
    builder
        .build_return(Some(&cast))
        .map_err(|error| CompileError::Codegen(error.to_string()))?;
    Ok(wrapper)
}

fn build_test<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    test: FunctionValue<'ctx>,
    foo: FunctionValue<'ctx>,
    alloc_slow: FunctionValue<'ctx>,
) -> Result<(), CompileError> {
    let obj = test
        .get_first_param()
        .ok_or_else(|| CompileError::Codegen("missing gc pointer parameter".into()))?
        .into_pointer_value();
    let entry = context.append_basic_block(test, "entry");
    builder.position_at_end(entry);
    builder
        .build_direct_call(foo, &[obj.into()], "")
        .map_err(|error| CompileError::Codegen(error.to_string()))?;
    let _allocated = builder
        .build_direct_call(
            alloc_slow,
            &[
                context.i64_type().const_int(24, false).into(),
                context.i64_type().const_int(8, false).into(),
                context.i16_type().const_int(1, false).into(),
            ],
            "raw_alloc",
        )
        .map_err(|error| CompileError::Codegen(error.to_string()))?
        .try_as_basic_value()
        .basic()
        .ok_or_else(|| CompileError::Codegen("rt_alloc_slow must return a pointer".into()))?
        .into_pointer_value();
    builder
        .build_return(Some(&obj))
        .map_err(|error| CompileError::Codegen(error.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        GC_HEAP_ADDRSPACE, GC_STRATEGY_NAME, attach_gc_strategy, compile_pre_statepoint_example,
        gc_ptr_type,
    };
    use inkwell::context::Context;

    #[test]
    fn uses_addrspace_one_for_gc_heap() {
        let context = Context::create();
        assert_eq!(
            gc_ptr_type(&context).get_address_space(),
            inkwell::AddressSpace::from(GC_HEAP_ADDRSPACE)
        );
    }

    #[test]
    fn attaches_gc_strategy_name() {
        let context = Context::create();
        let module = context.create_module("gc_strategy");
        let function = module.add_function("f", context.void_type().fn_type(&[], false), None);
        attach_gc_strategy(function);
        assert_eq!(function.get_gc().to_str(), Ok(GC_STRATEGY_NAME));
    }

    #[test]
    fn compiles_pre_statepoint_example_module() {
        let module = compile_pre_statepoint_example("gc_example").unwrap();
        assert!(module.llvm_ir.contains("define void @gc.safepoint_poll()"));
        assert!(module.llvm_ir.contains("declare void @gc_safepoint_poll()"));
        assert!(module.llvm_ir.contains("declare ptr @rt_alloc_slow(i64, i64, i16)"));
        assert!(module.llvm_ir.contains("declare void @rt_gc_poll()"));
        assert!(
            module
                .llvm_ir
                .contains("define ptr addrspace(1) @test(ptr addrspace(1) %0)")
        );
        assert!(module.llvm_ir.contains("gc \"coreclr\""));
    }
}
