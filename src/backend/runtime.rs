use crate::backend::statepoint::heap_address_space;
use inkwell::module::Module;
use inkwell::AddressSpace;
use inkwell::values::FunctionValue;

pub struct RuntimeAbi<'ctx> {
    pub rt_mmtk_init: FunctionValue<'ctx>,
    pub rt_bind_thread: FunctionValue<'ctx>,
    pub rt_unbind_thread: FunctionValue<'ctx>,
    pub rt_alloc_slow: FunctionValue<'ctx>,
    pub rt_gc_poll: FunctionValue<'ctx>,
    pub rt_exception_pending: FunctionValue<'ctx>,
    pub rt_raise: FunctionValue<'ctx>,
    pub rt_take_pending_exception: FunctionValue<'ctx>,
    pub rt_object_write_post: FunctionValue<'ctx>,
    pub rt_root_slot_push: FunctionValue<'ctx>,
    pub rt_root_slot_pop: FunctionValue<'ctx>,
    pub gc_safepoint_poll: FunctionValue<'ctx>,
    pub make_fixnum: FunctionValue<'ctx>,
    pub make_bool: FunctionValue<'ctx>,
    pub empty_list: FunctionValue<'ctx>,
    pub unspecified: FunctionValue<'ctx>,
    pub gc_stress: FunctionValue<'ctx>,
    pub display: FunctionValue<'ctx>,
    pub write: FunctionValue<'ctx>,
    pub newline: FunctionValue<'ctx>,
    pub alloc_pair: FunctionValue<'ctx>,
    pub alloc_pair_gc: FunctionValue<'ctx>,
    pub pair_car: FunctionValue<'ctx>,
    pub pair_cdr: FunctionValue<'ctx>,
    pub pair_car_gc: FunctionValue<'ctx>,
    pub pair_cdr_gc: FunctionValue<'ctx>,
    pub pair_set_car: FunctionValue<'ctx>,
    pub pair_set_cdr: FunctionValue<'ctx>,
    pub pair_set_car_gc: FunctionValue<'ctx>,
    pub pair_set_cdr_gc: FunctionValue<'ctx>,
    pub is_pair: FunctionValue<'ctx>,
    pub is_list: FunctionValue<'ctx>,
    pub list_length: FunctionValue<'ctx>,
    pub list_tail: FunctionValue<'ctx>,
    pub list_ref: FunctionValue<'ctx>,
    pub append: FunctionValue<'ctx>,
    pub memq: FunctionValue<'ctx>,
    pub memv: FunctionValue<'ctx>,
    pub member: FunctionValue<'ctx>,
    pub assq: FunctionValue<'ctx>,
    pub assv: FunctionValue<'ctx>,
    pub assoc: FunctionValue<'ctx>,
    pub list_copy: FunctionValue<'ctx>,
    pub reverse: FunctionValue<'ctx>,
    pub alloc_box: FunctionValue<'ctx>,
    pub alloc_box_gc: FunctionValue<'ctx>,
    pub box_set_gc: FunctionValue<'ctx>,
    pub alloc_promise: FunctionValue<'ctx>,
    pub alloc_promise_gc: FunctionValue<'ctx>,
    pub promise_forced: FunctionValue<'ctx>,
    pub promise_value: FunctionValue<'ctx>,
    pub promise_resolve: FunctionValue<'ctx>,
    pub promise_forced_gc: FunctionValue<'ctx>,
    pub promise_value_gc: FunctionValue<'ctx>,
    pub promise_resolve_gc: FunctionValue<'ctx>,
    pub alloc_closure: FunctionValue<'ctx>,
    pub alloc_closure_gc: FunctionValue<'ctx>,
    pub closure_code_ptr_gc: FunctionValue<'ctx>,
    pub closure_env_ref_gc: FunctionValue<'ctx>,
    pub closure_env_set_gc: FunctionValue<'ctx>,
    pub alloc_string: FunctionValue<'ctx>,
    pub alloc_string_gc: FunctionValue<'ctx>,
    pub alloc_symbol: FunctionValue<'ctx>,
    pub alloc_symbol_gc: FunctionValue<'ctx>,
    pub is_symbol: FunctionValue<'ctx>,
    pub alloc_values: FunctionValue<'ctx>,
    pub is_values: FunctionValue<'ctx>,
    pub values_length: FunctionValue<'ctx>,
    pub values_ref: FunctionValue<'ctx>,
    pub values_tail_list: FunctionValue<'ctx>,
    pub equal: FunctionValue<'ctx>,
    pub apply_builtin: FunctionValue<'ctx>,
    pub symbol_to_string: FunctionValue<'ctx>,
    pub string_to_symbol: FunctionValue<'ctx>,
    pub is_string: FunctionValue<'ctx>,
    pub string_length: FunctionValue<'ctx>,
    pub string_ref: FunctionValue<'ctx>,
    pub string_length_gc: FunctionValue<'ctx>,
    pub string_ref_gc: FunctionValue<'ctx>,
    pub alloc_vector: FunctionValue<'ctx>,
    pub alloc_vector_gc: FunctionValue<'ctx>,
    pub is_vector: FunctionValue<'ctx>,
    pub vector_length: FunctionValue<'ctx>,
    pub vector_ref: FunctionValue<'ctx>,
    pub vector_set: FunctionValue<'ctx>,
    pub vector_length_gc: FunctionValue<'ctx>,
    pub vector_ref_gc: FunctionValue<'ctx>,
    pub vector_set_gc: FunctionValue<'ctx>,
}

impl<'ctx> RuntimeAbi<'ctx> {
    pub fn declare(module: &Module<'ctx>) -> Self {
        let context = module.get_context();
        let word = context.i64_type();
        let i16_ty = context.i16_type();
        let bool_ty = context.bool_type();
        let raw_ptr = context.ptr_type(AddressSpace::default());
        let usize_fn = word.fn_type(&[], false);
        let word_word_fn = word.fn_type(&[word.into()], false);
        let pair_fn = word.fn_type(&[word.into(), word.into()], false);
        let raw_bytes_string_fn = word.fn_type(&[raw_ptr.into(), word.into()], false);
        let raw_words_vector_fn = word.fn_type(&[raw_ptr.into(), word.into()], false);
        let raw_words_closure_fn = raw_ptr.fn_type(&[word.into(), raw_ptr.into(), word.into()], false);
        let raw_pair_alloc_fn = raw_ptr.fn_type(&[word.into(), word.into()], false);
        let raw_pair_access_fn = word.fn_type(&[raw_ptr.into()], false);
        let raw_ptr_index_access_fn = word.fn_type(&[raw_ptr.into(), word.into()], false);

        let alloc_pair_gc_raw =
            module.add_function("mlisp_alloc_pair_gc", raw_pair_alloc_fn, None);
        let pair_car_gc_raw = module.add_function("mlisp_pair_car_gc", raw_pair_access_fn, None);
        let pair_cdr_gc_raw = module.add_function("mlisp_pair_cdr_gc", raw_pair_access_fn, None);
        let pair_set_car_gc_raw = module.add_function(
            "mlisp_pair_set_car_gc",
            word.fn_type(&[raw_ptr.into(), word.into()], false),
            None,
        );
        let pair_set_cdr_gc_raw = module.add_function(
            "mlisp_pair_set_cdr_gc",
            word.fn_type(&[raw_ptr.into(), word.into()], false),
            None,
        );
        let alloc_string_gc_raw = module.add_function(
            "mlisp_alloc_string_gc",
            raw_ptr.fn_type(&[raw_ptr.into(), word.into()], false),
            None,
        );
        let alloc_symbol_gc_raw = module.add_function(
            "mlisp_alloc_symbol_gc",
            raw_ptr.fn_type(&[raw_ptr.into(), word.into()], false),
            None,
        );
        let alloc_box_gc_raw =
            module.add_function("mlisp_alloc_box_gc", raw_ptr.fn_type(&[word.into()], false), None);
        let alloc_promise_gc_raw =
            module.add_function("mlisp_alloc_promise_gc", raw_ptr.fn_type(&[word.into()], false), None);
        let box_set_gc_raw = module.add_function(
            "mlisp_box_set_gc",
            word.fn_type(&[raw_ptr.into(), word.into()], false),
            None,
        );
        let promise_forced_gc_raw = module.add_function(
            "mlisp_promise_forced_gc",
            bool_ty.fn_type(&[raw_ptr.into()], false),
            None,
        );
        let promise_value_gc_raw = module.add_function(
            "mlisp_promise_value_gc",
            word.fn_type(&[raw_ptr.into()], false),
            None,
        );
        let promise_resolve_gc_raw = module.add_function(
            "mlisp_promise_resolve_gc",
            word.fn_type(&[raw_ptr.into(), word.into()], false),
            None,
        );
        let alloc_closure_gc_raw =
            module.add_function("mlisp_alloc_closure_gc", raw_words_closure_fn, None);
        let closure_code_ptr_gc_raw =
            module.add_function("mlisp_closure_code_ptr_gc", word.fn_type(&[raw_ptr.into()], false), None);
        let closure_env_ref_gc_raw =
            module.add_function("mlisp_closure_env_ref_gc", raw_ptr_index_access_fn, None);
        let closure_env_set_gc_raw = module.add_function(
            "mlisp_closure_env_set_gc",
            word.fn_type(&[raw_ptr.into(), word.into(), word.into()], false),
            None,
        );
        let string_length_gc_raw = module.add_function(
            "mlisp_string_length_gc",
            word.fn_type(&[raw_ptr.into()], false),
            None,
        );
        let string_ref_gc_raw = module.add_function(
            "mlisp_string_ref_gc",
            word.fn_type(&[raw_ptr.into(), word.into()], false),
            None,
        );
        let alloc_vector_gc_raw = module.add_function(
            "mlisp_alloc_vector_gc",
            raw_ptr.fn_type(&[raw_ptr.into(), word.into()], false),
            None,
        );
        let vector_length_gc_raw = module.add_function(
            "mlisp_vector_length_gc",
            word.fn_type(&[raw_ptr.into()], false),
            None,
        );
        let vector_ref_gc_raw = module.add_function(
            "mlisp_vector_ref_gc",
            word.fn_type(&[raw_ptr.into(), word.into()], false),
            None,
        );
        let vector_set_gc_raw = module.add_function(
            "mlisp_vector_set_gc",
            word.fn_type(&[raw_ptr.into(), word.into(), word.into()], false),
            None,
        );

        let alloc_pair_gc = add_gc_alloc_wrapper(module, "__mlisp_alloc_pair_gc_as1", alloc_pair_gc_raw);
        let pair_car_gc = add_gc_unary_wrapper(module, "__mlisp_pair_car_gc_as1", pair_car_gc_raw);
        let pair_cdr_gc = add_gc_unary_wrapper(module, "__mlisp_pair_cdr_gc_as1", pair_cdr_gc_raw);
        let pair_set_car_gc =
            add_gc_unary_value_wrapper(module, "__mlisp_pair_set_car_gc_as1", pair_set_car_gc_raw);
        let pair_set_cdr_gc =
            add_gc_unary_value_wrapper(module, "__mlisp_pair_set_cdr_gc_as1", pair_set_cdr_gc_raw);
        let alloc_string_gc =
            add_gc_buffer_alloc_wrapper(module, "__mlisp_alloc_string_gc_as1", alloc_string_gc_raw);
        let alloc_symbol_gc =
            add_gc_buffer_alloc_wrapper(module, "__mlisp_alloc_symbol_gc_as1", alloc_symbol_gc_raw);
        let alloc_box_gc = add_gc_unary_alloc_wrapper(module, "__mlisp_alloc_box_gc_as1", alloc_box_gc_raw);
        let alloc_promise_gc =
            add_gc_unary_alloc_wrapper(module, "__mlisp_alloc_promise_gc_as1", alloc_promise_gc_raw);
        let box_set_gc = add_gc_unary_value_wrapper(module, "__mlisp_box_set_gc_as1", box_set_gc_raw);
        let promise_forced_gc =
            add_gc_unary_bool_wrapper(module, "__mlisp_promise_forced_gc_as1", promise_forced_gc_raw);
        let promise_value_gc =
            add_gc_unary_wrapper(module, "__mlisp_promise_value_gc_as1", promise_value_gc_raw);
        let promise_resolve_gc =
            add_gc_unary_value_wrapper(module, "__mlisp_promise_resolve_gc_as1", promise_resolve_gc_raw);
        let alloc_closure_gc =
            add_gc_code_buffer_alloc_wrapper(module, "__mlisp_alloc_closure_gc_as1", alloc_closure_gc_raw);
        let closure_code_ptr_gc =
            add_gc_unary_wrapper(module, "__mlisp_closure_code_ptr_gc_as1", closure_code_ptr_gc_raw);
        let closure_env_ref_gc =
            add_gc_index_wrapper(module, "__mlisp_closure_env_ref_gc_as1", closure_env_ref_gc_raw);
        let closure_env_set_gc =
            add_gc_index_value_wrapper(module, "__mlisp_closure_env_set_gc_as1", closure_env_set_gc_raw);
        let string_length_gc =
            add_gc_unary_wrapper(module, "__mlisp_string_length_gc_as1", string_length_gc_raw);
        let string_ref_gc = add_gc_index_wrapper(module, "__mlisp_string_ref_gc_as1", string_ref_gc_raw);
        let alloc_vector_gc =
            add_gc_buffer_alloc_wrapper(module, "__mlisp_alloc_vector_gc_as1", alloc_vector_gc_raw);
        let vector_length_gc =
            add_gc_unary_wrapper(module, "__mlisp_vector_length_gc_as1", vector_length_gc_raw);
        let vector_ref_gc = add_gc_index_wrapper(module, "__mlisp_vector_ref_gc_as1", vector_ref_gc_raw);
        let vector_set_gc =
            add_gc_index_value_wrapper(module, "__mlisp_vector_set_gc_as1", vector_set_gc_raw);
        add_poll_wrapper(module);

        Self {
            rt_mmtk_init: module.add_function(
                "rt_mmtk_init",
                bool_ty.fn_type(&[word.into(), word.into()], false),
                None,
            ),
            rt_bind_thread: module.add_function("rt_bind_thread", raw_ptr.fn_type(&[], false), None),
            rt_unbind_thread: module.add_function(
                "rt_unbind_thread",
                context.void_type().fn_type(&[raw_ptr.into()], false),
                None,
            ),
            rt_alloc_slow: module.add_function(
                "rt_alloc_slow",
                raw_ptr.fn_type(&[word.into(), word.into(), i16_ty.into()], false),
                None,
            ),
            rt_gc_poll: module.add_function("rt_gc_poll", context.void_type().fn_type(&[], false), None),
            rt_exception_pending: module.add_function(
                "rt_exception_pending",
                bool_ty.fn_type(&[], false),
                None,
            ),
            rt_raise: module.add_function("rt_raise", word.fn_type(&[word.into()], false), None),
            rt_take_pending_exception: module.add_function(
                "rt_take_pending_exception",
                word.fn_type(&[], false),
                None,
            ),
            rt_object_write_post: module.add_function(
                "rt_object_write_post",
                context
                    .void_type()
                    .fn_type(&[raw_ptr.into(), raw_ptr.into(), word.into()], false),
                None,
            ),
            rt_root_slot_push: module.add_function(
                "rt_root_slot_push",
                context.void_type().fn_type(&[raw_ptr.into()], false),
                None,
            ),
            rt_root_slot_pop: module.add_function(
                "rt_root_slot_pop",
                context.void_type().fn_type(&[], false),
                None,
            ),
            gc_safepoint_poll: module.get_function("gc_safepoint_poll").unwrap_or_else(|| {
                module.add_function(
                    "gc_safepoint_poll",
                    context.void_type().fn_type(&[], false),
                    None,
                )
            }),
            make_fixnum: module.add_function(
                "mlisp_make_fixnum",
                word.fn_type(&[word.into()], false),
                None,
            ),
            make_bool: module.add_function(
                "mlisp_make_bool",
                word.fn_type(&[bool_ty.into()], false),
                None,
            ),
            empty_list: module.add_function("mlisp_empty_list", usize_fn, None),
            unspecified: module.add_function("mlisp_unspecified", usize_fn, None),
            gc_stress: module.add_function("mlisp_gc_stress", word_word_fn, None),
            display: module.add_function("mlisp_display", word_word_fn, None),
            write: module.add_function("mlisp_write", word_word_fn, None),
            newline: module.add_function("mlisp_newline", usize_fn, None),
            alloc_pair: module.add_function("mlisp_alloc_pair", pair_fn, None),
            alloc_pair_gc,
            pair_car: module.add_function("mlisp_pair_car", word_word_fn, None),
            pair_cdr: module.add_function("mlisp_pair_cdr", word_word_fn, None),
            pair_car_gc,
            pair_cdr_gc,
            pair_set_car: module.add_function("mlisp_pair_set_car", pair_fn, None),
            pair_set_cdr: module.add_function("mlisp_pair_set_cdr", pair_fn, None),
            pair_set_car_gc,
            pair_set_cdr_gc,
            is_pair: module.add_function(
                "mlisp_is_pair",
                bool_ty.fn_type(&[word.into()], false),
                None,
            ),
            is_list: module.add_function(
                "mlisp_is_list",
                bool_ty.fn_type(&[word.into()], false),
                None,
            ),
            list_length: module.add_function("mlisp_list_length", word_word_fn, None),
            list_tail: module.add_function("mlisp_list_tail", pair_fn, None),
            list_ref: module.add_function("mlisp_list_ref", pair_fn, None),
            append: module.add_function("mlisp_append", pair_fn, None),
            memq: module.add_function("mlisp_memq", pair_fn, None),
            memv: module.add_function("mlisp_memv", pair_fn, None),
            member: module.add_function("mlisp_member", pair_fn, None),
            assq: module.add_function("mlisp_assq", pair_fn, None),
            assv: module.add_function("mlisp_assv", pair_fn, None),
            assoc: module.add_function("mlisp_assoc", pair_fn, None),
            list_copy: module.add_function("mlisp_list_copy", word_word_fn, None),
            reverse: module.add_function("mlisp_reverse", word_word_fn, None),
            alloc_box: module.add_function("mlisp_alloc_box", word_word_fn, None),
            alloc_box_gc,
            box_set_gc,
            alloc_promise: module.add_function("mlisp_alloc_promise", word_word_fn, None),
            alloc_promise_gc,
            promise_forced: module.add_function(
                "mlisp_promise_forced",
                bool_ty.fn_type(&[word.into()], false),
                None,
            ),
            promise_value: module.add_function("mlisp_promise_value", word_word_fn, None),
            promise_resolve: module.add_function(
                "mlisp_promise_resolve",
                word.fn_type(&[word.into(), word.into()], false),
                None,
            ),
            promise_forced_gc,
            promise_value_gc,
            promise_resolve_gc,
            alloc_closure: module.add_function(
                "mlisp_alloc_closure",
                word.fn_type(&[word.into(), raw_ptr.into(), word.into()], false),
                None,
            ),
            alloc_closure_gc,
            closure_code_ptr_gc,
            closure_env_ref_gc,
            closure_env_set_gc,
            alloc_string: module.add_function("mlisp_alloc_string", raw_bytes_string_fn, None),
            alloc_string_gc,
            alloc_symbol: module.add_function("mlisp_alloc_symbol", raw_bytes_string_fn, None),
            alloc_symbol_gc,
            is_symbol: module.add_function(
                "mlisp_is_symbol",
                bool_ty.fn_type(&[word.into()], false),
                None,
            ),
            alloc_values: module.add_function("mlisp_alloc_values", raw_words_vector_fn, None),
            is_values: module.add_function(
                "mlisp_is_values",
                bool_ty.fn_type(&[word.into()], false),
                None,
            ),
            values_length: module.add_function("mlisp_values_length", word_word_fn, None),
            values_ref: module.add_function("mlisp_values_ref", pair_fn, None),
            values_tail_list: module.add_function("mlisp_values_tail_list", pair_fn, None),
            equal: module.add_function(
                "mlisp_equal",
                bool_ty.fn_type(&[word.into(), word.into()], false),
                None,
            ),
            apply_builtin: module.add_function(
                "mlisp_apply_builtin",
                word.fn_type(&[word.into(), word.into()], false),
                None,
            ),
            symbol_to_string: module.add_function("mlisp_symbol_to_string", word_word_fn, None),
            string_to_symbol: module.add_function("mlisp_string_to_symbol", word_word_fn, None),
            is_string: module.add_function(
                "mlisp_is_string",
                bool_ty.fn_type(&[word.into()], false),
                None,
            ),
            string_length: module.add_function("mlisp_string_length", word_word_fn, None),
            string_ref: module.add_function("mlisp_string_ref", pair_fn, None),
            string_length_gc,
            string_ref_gc,
            alloc_vector: module.add_function("mlisp_alloc_vector", raw_words_vector_fn, None),
            alloc_vector_gc,
            is_vector: module.add_function(
                "mlisp_is_vector",
                bool_ty.fn_type(&[word.into()], false),
                None,
            ),
            vector_length: module.add_function("mlisp_vector_length", word_word_fn, None),
            vector_ref: module.add_function("mlisp_vector_ref", pair_fn, None),
            vector_set: module.add_function(
                "mlisp_vector_set",
                word.fn_type(&[word.into(), word.into(), word.into()], false),
                None,
            ),
            vector_length_gc,
            vector_ref_gc,
            vector_set_gc,
        }
    }
}

fn add_poll_wrapper(module: &Module<'_>) {
    if module.get_function("gc.safepoint_poll").is_some() {
        return;
    }
    let context = module.get_context();
    let builder = context.create_builder();
    let wrapper = module.add_function(
        "gc.safepoint_poll",
        context.void_type().fn_type(&[], false),
        None,
    );
    let runtime_poll = module.get_function("gc_safepoint_poll").unwrap_or_else(|| {
        module.add_function(
            "gc_safepoint_poll",
            context.void_type().fn_type(&[], false),
            None,
        )
    });
    let entry = context.append_basic_block(wrapper, "entry");
    builder.position_at_end(entry);
    builder.build_call(runtime_poll, &[], "").unwrap();
    builder.build_return(None).unwrap();
}

fn add_gc_alloc_wrapper<'ctx>(
    module: &Module<'ctx>,
    name: &str,
    raw_target: FunctionValue<'ctx>,
) -> FunctionValue<'ctx> {
    let context = module.get_context();
    let builder = context.create_builder();
    let gc_ptr = context.ptr_type(heap_address_space());
    let word = context.i64_type();
    let wrapper = module.add_function(name, gc_ptr.fn_type(&[word.into(), word.into()], false), None);
    let entry = context.append_basic_block(wrapper, "entry");
    builder.position_at_end(entry);
    let arg0 = wrapper.get_nth_param(0).unwrap().into_int_value();
    let arg1 = wrapper.get_nth_param(1).unwrap().into_int_value();
    let raw = builder
        .build_call(raw_target, &[arg0.into(), arg1.into()], "raw")
        .unwrap()
        .try_as_basic_value()
        .basic()
        .unwrap()
        .into_pointer_value();
    let cast = builder
        .build_address_space_cast(raw, gc_ptr, "gc")
        .unwrap();
    builder.build_return(Some(&cast)).unwrap();
    wrapper
}

fn add_gc_buffer_alloc_wrapper<'ctx>(
    module: &Module<'ctx>,
    name: &str,
    raw_target: FunctionValue<'ctx>,
) -> FunctionValue<'ctx> {
    let context = module.get_context();
    let builder = context.create_builder();
    let raw_ptr = context.ptr_type(AddressSpace::default());
    let gc_ptr = context.ptr_type(heap_address_space());
    let word = context.i64_type();
    let wrapper = module.add_function(name, gc_ptr.fn_type(&[raw_ptr.into(), word.into()], false), None);
    let entry = context.append_basic_block(wrapper, "entry");
    builder.position_at_end(entry);
    let arg0 = wrapper.get_nth_param(0).unwrap().into_pointer_value();
    let arg1 = wrapper.get_nth_param(1).unwrap().into_int_value();
    let raw = builder
        .build_call(raw_target, &[arg0.into(), arg1.into()], "raw")
        .unwrap()
        .try_as_basic_value()
        .basic()
        .unwrap()
        .into_pointer_value();
    let cast = builder
        .build_address_space_cast(raw, gc_ptr, "gc")
        .unwrap();
    builder.build_return(Some(&cast)).unwrap();
    wrapper
}

fn add_gc_unary_alloc_wrapper<'ctx>(
    module: &Module<'ctx>,
    name: &str,
    raw_target: FunctionValue<'ctx>,
) -> FunctionValue<'ctx> {
    let context = module.get_context();
    let builder = context.create_builder();
    let gc_ptr = context.ptr_type(heap_address_space());
    let word = context.i64_type();
    let wrapper = module.add_function(name, gc_ptr.fn_type(&[word.into()], false), None);
    let entry = context.append_basic_block(wrapper, "entry");
    builder.position_at_end(entry);
    let arg0 = wrapper.get_nth_param(0).unwrap().into_int_value();
    let raw = builder
        .build_call(raw_target, &[arg0.into()], "raw")
        .unwrap()
        .try_as_basic_value()
        .basic()
        .unwrap()
        .into_pointer_value();
    let cast = builder.build_address_space_cast(raw, gc_ptr, "gc").unwrap();
    builder.build_return(Some(&cast)).unwrap();
    wrapper
}

fn add_gc_code_buffer_alloc_wrapper<'ctx>(
    module: &Module<'ctx>,
    name: &str,
    raw_target: FunctionValue<'ctx>,
) -> FunctionValue<'ctx> {
    let context = module.get_context();
    let builder = context.create_builder();
    let raw_ptr = context.ptr_type(AddressSpace::default());
    let gc_ptr = context.ptr_type(heap_address_space());
    let word = context.i64_type();
    let wrapper = module.add_function(
        name,
        gc_ptr.fn_type(&[word.into(), raw_ptr.into(), word.into()], false),
        None,
    );
    let entry = context.append_basic_block(wrapper, "entry");
    builder.position_at_end(entry);
    let arg0 = wrapper.get_nth_param(0).unwrap().into_int_value();
    let arg1 = wrapper.get_nth_param(1).unwrap().into_pointer_value();
    let arg2 = wrapper.get_nth_param(2).unwrap().into_int_value();
    let raw = builder
        .build_call(raw_target, &[arg0.into(), arg1.into(), arg2.into()], "raw")
        .unwrap()
        .try_as_basic_value()
        .basic()
        .unwrap()
        .into_pointer_value();
    let cast = builder
        .build_address_space_cast(raw, gc_ptr, "gc")
        .unwrap();
    builder.build_return(Some(&cast)).unwrap();
    wrapper
}

fn add_gc_unary_wrapper<'ctx>(
    module: &Module<'ctx>,
    name: &str,
    raw_target: FunctionValue<'ctx>,
) -> FunctionValue<'ctx> {
    let context = module.get_context();
    let builder = context.create_builder();
    let gc_ptr = context.ptr_type(heap_address_space());
    let raw_ptr = context.ptr_type(AddressSpace::default());
    let word = context.i64_type();
    let wrapper = module.add_function(name, word.fn_type(&[gc_ptr.into()], false), None);
    let entry = context.append_basic_block(wrapper, "entry");
    builder.position_at_end(entry);
    let arg0 = wrapper.get_nth_param(0).unwrap().into_pointer_value();
    let raw_arg = builder
        .build_address_space_cast(arg0, raw_ptr, "raw")
        .unwrap();
    let result = builder
        .build_call(raw_target, &[raw_arg.into()], "result")
        .unwrap()
        .try_as_basic_value()
        .basic()
        .unwrap()
        .into_int_value();
    builder.build_return(Some(&result)).unwrap();
    wrapper
}

fn add_gc_unary_bool_wrapper<'ctx>(
    module: &Module<'ctx>,
    name: &str,
    raw_target: FunctionValue<'ctx>,
) -> FunctionValue<'ctx> {
    let context = module.get_context();
    let builder = context.create_builder();
    let gc_ptr = context.ptr_type(heap_address_space());
    let raw_ptr = context.ptr_type(AddressSpace::default());
    let bool_ty = context.bool_type();
    let wrapper = module.add_function(name, bool_ty.fn_type(&[gc_ptr.into()], false), None);
    let entry = context.append_basic_block(wrapper, "entry");
    builder.position_at_end(entry);
    let arg0 = wrapper.get_nth_param(0).unwrap().into_pointer_value();
    let raw_arg = builder
        .build_address_space_cast(arg0, raw_ptr, "raw")
        .unwrap();
    let result = builder
        .build_call(raw_target, &[raw_arg.into()], "result")
        .unwrap()
        .try_as_basic_value()
        .basic()
        .unwrap()
        .into_int_value();
    builder.build_return(Some(&result)).unwrap();
    wrapper
}

fn add_gc_index_wrapper<'ctx>(
    module: &Module<'ctx>,
    name: &str,
    raw_target: FunctionValue<'ctx>,
) -> FunctionValue<'ctx> {
    let context = module.get_context();
    let builder = context.create_builder();
    let gc_ptr = context.ptr_type(heap_address_space());
    let raw_ptr = context.ptr_type(AddressSpace::default());
    let word = context.i64_type();
    let wrapper = module.add_function(name, word.fn_type(&[gc_ptr.into(), word.into()], false), None);
    let entry = context.append_basic_block(wrapper, "entry");
    builder.position_at_end(entry);
    let arg0 = wrapper.get_nth_param(0).unwrap().into_pointer_value();
    let arg1 = wrapper.get_nth_param(1).unwrap().into_int_value();
    let raw_arg = builder
        .build_address_space_cast(arg0, raw_ptr, "raw")
        .unwrap();
    let result = builder
        .build_call(raw_target, &[raw_arg.into(), arg1.into()], "result")
        .unwrap()
        .try_as_basic_value()
        .basic()
        .unwrap()
        .into_int_value();
    builder.build_return(Some(&result)).unwrap();
    wrapper
}

fn add_gc_index_value_wrapper<'ctx>(
    module: &Module<'ctx>,
    name: &str,
    raw_target: FunctionValue<'ctx>,
) -> FunctionValue<'ctx> {
    let context = module.get_context();
    let builder = context.create_builder();
    let gc_ptr = context.ptr_type(heap_address_space());
    let raw_ptr = context.ptr_type(AddressSpace::default());
    let word = context.i64_type();
    let wrapper = module.add_function(
        name,
        word.fn_type(&[gc_ptr.into(), word.into(), word.into()], false),
        None,
    );
    let entry = context.append_basic_block(wrapper, "entry");
    builder.position_at_end(entry);
    let arg0 = wrapper.get_nth_param(0).unwrap().into_pointer_value();
    let arg1 = wrapper.get_nth_param(1).unwrap().into_int_value();
    let arg2 = wrapper.get_nth_param(2).unwrap().into_int_value();
    let raw_arg = builder
        .build_address_space_cast(arg0, raw_ptr, "raw")
        .unwrap();
    let result = builder
        .build_call(raw_target, &[raw_arg.into(), arg1.into(), arg2.into()], "result")
        .unwrap()
        .try_as_basic_value()
        .basic()
        .unwrap()
        .into_int_value();
    builder.build_return(Some(&result)).unwrap();
    wrapper
}

fn add_gc_unary_value_wrapper<'ctx>(
    module: &Module<'ctx>,
    name: &str,
    raw_target: FunctionValue<'ctx>,
) -> FunctionValue<'ctx> {
    let context = module.get_context();
    let builder = context.create_builder();
    let gc_ptr = context.ptr_type(heap_address_space());
    let raw_ptr = context.ptr_type(AddressSpace::default());
    let word = context.i64_type();
    let wrapper = module.add_function(name, word.fn_type(&[gc_ptr.into(), word.into()], false), None);
    let entry = context.append_basic_block(wrapper, "entry");
    builder.position_at_end(entry);
    let arg0 = wrapper.get_nth_param(0).unwrap().into_pointer_value();
    let arg1 = wrapper.get_nth_param(1).unwrap().into_int_value();
    let raw_arg = builder.build_address_space_cast(arg0, raw_ptr, "raw").unwrap();
    let result = builder
        .build_call(raw_target, &[raw_arg.into(), arg1.into()], "result")
        .unwrap()
        .try_as_basic_value()
        .basic()
        .unwrap()
        .into_int_value();
    builder.build_return(Some(&result)).unwrap();
    wrapper
}

#[cfg(test)]
mod tests {
    use super::RuntimeAbi;
    use inkwell::context::Context;

    #[test]
    fn declares_runtime_symbols() {
        let context = Context::create();
        let module = context.create_module("runtime_abi");
        let abi = RuntimeAbi::declare(&module);

        assert_eq!(abi.rt_mmtk_init.get_name().to_str(), Ok("rt_mmtk_init"));
        assert_eq!(abi.rt_bind_thread.get_name().to_str(), Ok("rt_bind_thread"));
        assert_eq!(abi.rt_unbind_thread.get_name().to_str(), Ok("rt_unbind_thread"));
        assert_eq!(abi.rt_alloc_slow.get_name().to_str(), Ok("rt_alloc_slow"));
        assert_eq!(abi.rt_gc_poll.get_name().to_str(), Ok("rt_gc_poll"));
        assert_eq!(
            abi.rt_exception_pending.get_name().to_str(),
            Ok("rt_exception_pending")
        );
        assert_eq!(abi.rt_raise.get_name().to_str(), Ok("rt_raise"));
        assert_eq!(
            abi.rt_take_pending_exception.get_name().to_str(),
            Ok("rt_take_pending_exception")
        );
        assert_eq!(
            abi.rt_object_write_post.get_name().to_str(),
            Ok("rt_object_write_post")
        );
        assert_eq!(abi.rt_root_slot_push.get_name().to_str(), Ok("rt_root_slot_push"));
        assert_eq!(abi.rt_root_slot_pop.get_name().to_str(), Ok("rt_root_slot_pop"));
        assert_eq!(
            abi.gc_safepoint_poll.get_name().to_str(),
            Ok("gc_safepoint_poll")
        );
        assert_eq!(abi.make_fixnum.get_name().to_str(), Ok("mlisp_make_fixnum"));
        assert_eq!(abi.gc_stress.get_name().to_str(), Ok("mlisp_gc_stress"));
        assert_eq!(abi.display.get_name().to_str(), Ok("mlisp_display"));
        assert_eq!(abi.write.get_name().to_str(), Ok("mlisp_write"));
        assert_eq!(abi.newline.get_name().to_str(), Ok("mlisp_newline"));
        assert_eq!(abi.alloc_pair.get_name().to_str(), Ok("mlisp_alloc_pair"));
        assert_eq!(abi.alloc_pair_gc.get_name().to_str(), Ok("__mlisp_alloc_pair_gc_as1"));
        assert_eq!(abi.pair_car.get_name().to_str(), Ok("mlisp_pair_car"));
        assert_eq!(abi.pair_cdr.get_name().to_str(), Ok("mlisp_pair_cdr"));
        assert_eq!(abi.pair_car_gc.get_name().to_str(), Ok("__mlisp_pair_car_gc_as1"));
        assert_eq!(abi.pair_cdr_gc.get_name().to_str(), Ok("__mlisp_pair_cdr_gc_as1"));
        assert_eq!(abi.pair_set_car.get_name().to_str(), Ok("mlisp_pair_set_car"));
        assert_eq!(abi.pair_set_cdr.get_name().to_str(), Ok("mlisp_pair_set_cdr"));
        assert_eq!(abi.pair_set_car_gc.get_name().to_str(), Ok("__mlisp_pair_set_car_gc_as1"));
        assert_eq!(abi.pair_set_cdr_gc.get_name().to_str(), Ok("__mlisp_pair_set_cdr_gc_as1"));
        assert_eq!(abi.is_pair.get_name().to_str(), Ok("mlisp_is_pair"));
        assert_eq!(abi.is_list.get_name().to_str(), Ok("mlisp_is_list"));
        assert_eq!(abi.list_length.get_name().to_str(), Ok("mlisp_list_length"));
        assert_eq!(abi.list_tail.get_name().to_str(), Ok("mlisp_list_tail"));
        assert_eq!(abi.list_ref.get_name().to_str(), Ok("mlisp_list_ref"));
        assert_eq!(abi.append.get_name().to_str(), Ok("mlisp_append"));
        assert_eq!(abi.memq.get_name().to_str(), Ok("mlisp_memq"));
        assert_eq!(abi.memv.get_name().to_str(), Ok("mlisp_memv"));
        assert_eq!(abi.member.get_name().to_str(), Ok("mlisp_member"));
        assert_eq!(abi.assq.get_name().to_str(), Ok("mlisp_assq"));
        assert_eq!(abi.assv.get_name().to_str(), Ok("mlisp_assv"));
        assert_eq!(abi.assoc.get_name().to_str(), Ok("mlisp_assoc"));
        assert_eq!(abi.list_copy.get_name().to_str(), Ok("mlisp_list_copy"));
        assert_eq!(abi.reverse.get_name().to_str(), Ok("mlisp_reverse"));
        assert_eq!(abi.alloc_box_gc.get_name().to_str(), Ok("__mlisp_alloc_box_gc_as1"));
        assert_eq!(abi.box_set_gc.get_name().to_str(), Ok("__mlisp_box_set_gc_as1"));
        assert_eq!(
            abi.alloc_closure.get_name().to_str(),
            Ok("mlisp_alloc_closure")
        );
        assert_eq!(abi.alloc_closure_gc.get_name().to_str(), Ok("__mlisp_alloc_closure_gc_as1"));
        assert_eq!(
            abi.closure_code_ptr_gc.get_name().to_str(),
            Ok("__mlisp_closure_code_ptr_gc_as1")
        );
        assert_eq!(
            abi.closure_env_ref_gc.get_name().to_str(),
            Ok("__mlisp_closure_env_ref_gc_as1")
        );
        assert_eq!(
            abi.closure_env_set_gc.get_name().to_str(),
            Ok("__mlisp_closure_env_set_gc_as1")
        );
        assert_eq!(abi.alloc_string.get_name().to_str(), Ok("mlisp_alloc_string"));
        assert_eq!(abi.alloc_string_gc.get_name().to_str(), Ok("__mlisp_alloc_string_gc_as1"));
        assert_eq!(abi.alloc_symbol.get_name().to_str(), Ok("mlisp_alloc_symbol"));
        assert_eq!(abi.alloc_symbol_gc.get_name().to_str(), Ok("__mlisp_alloc_symbol_gc_as1"));
        assert_eq!(abi.is_symbol.get_name().to_str(), Ok("mlisp_is_symbol"));
        assert_eq!(abi.alloc_values.get_name().to_str(), Ok("mlisp_alloc_values"));
        assert_eq!(abi.is_values.get_name().to_str(), Ok("mlisp_is_values"));
        assert_eq!(abi.values_length.get_name().to_str(), Ok("mlisp_values_length"));
        assert_eq!(abi.values_ref.get_name().to_str(), Ok("mlisp_values_ref"));
        assert_eq!(abi.values_tail_list.get_name().to_str(), Ok("mlisp_values_tail_list"));
        assert_eq!(abi.equal.get_name().to_str(), Ok("mlisp_equal"));
        assert_eq!(abi.apply_builtin.get_name().to_str(), Ok("mlisp_apply_builtin"));
        assert_eq!(abi.symbol_to_string.get_name().to_str(), Ok("mlisp_symbol_to_string"));
        assert_eq!(abi.string_to_symbol.get_name().to_str(), Ok("mlisp_string_to_symbol"));
        assert_eq!(abi.is_string.get_name().to_str(), Ok("mlisp_is_string"));
        assert_eq!(abi.string_length.get_name().to_str(), Ok("mlisp_string_length"));
        assert_eq!(abi.string_ref.get_name().to_str(), Ok("mlisp_string_ref"));
        assert_eq!(abi.string_length_gc.get_name().to_str(), Ok("__mlisp_string_length_gc_as1"));
        assert_eq!(abi.string_ref_gc.get_name().to_str(), Ok("__mlisp_string_ref_gc_as1"));
        assert_eq!(abi.alloc_vector.get_name().to_str(), Ok("mlisp_alloc_vector"));
        assert_eq!(abi.alloc_vector_gc.get_name().to_str(), Ok("__mlisp_alloc_vector_gc_as1"));
        assert_eq!(abi.is_vector.get_name().to_str(), Ok("mlisp_is_vector"));
        assert_eq!(
            abi.vector_length.get_name().to_str(),
            Ok("mlisp_vector_length")
        );
        assert_eq!(abi.vector_ref.get_name().to_str(), Ok("mlisp_vector_ref"));
        assert_eq!(abi.vector_set.get_name().to_str(), Ok("mlisp_vector_set"));
        assert_eq!(abi.vector_length_gc.get_name().to_str(), Ok("__mlisp_vector_length_gc_as1"));
        assert_eq!(abi.vector_ref_gc.get_name().to_str(), Ok("__mlisp_vector_ref_gc_as1"));
        assert_eq!(abi.vector_set_gc.get_name().to_str(), Ok("__mlisp_vector_set_gc_as1"));
    }
}
