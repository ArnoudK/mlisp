use crate::error::RuntimeError;
use crate::mmtk::{
    alloc_box_checked, alloc_closure_checked, alloc_pair_checked, alloc_raw_checked,
    alloc_string_checked, alloc_symbol_checked, alloc_vector_checked, bind_thread_handle, current_thread,
    gc_poll_current_checked, initialize_runtime, object_write_post_checked, pop_root_checked,
    push_root_checked, register_global_root_checked, run_mutator_stress_checked, gc_stress_checked,
    unbind_thread_handle,
};
use crate::object::{ClosureObject, PairObject, StringObject, SymbolObject, VectorObject};
use crate::value::Value;
use std::io::{self, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};

fn object_kind(value: Value) -> Result<u16, RuntimeError> {
    let object = value
        .to_object_reference()
        .ok_or(RuntimeError::InvalidObjectKind)?;
    Ok(unsafe { (*object.to_raw_address().to_ptr::<crate::layout::ObjectHeader>()).kind })
}

fn ffi_bool<F>(func: F) -> bool
where
    F: FnOnce() -> Result<bool, RuntimeError>,
{
    catch_unwind(AssertUnwindSafe(func))
        .ok()
        .and_then(Result::ok)
        .unwrap_or(false)
}

fn ffi_word<F>(func: F) -> usize
where
    F: FnOnce() -> Result<usize, RuntimeError>,
{
    catch_unwind(AssertUnwindSafe(func))
        .ok()
        .and_then(Result::ok)
        .unwrap_or_else(|| Value::unspecified().bits())
}

fn ffi_ptr<T, F>(func: F) -> *mut T
where
    F: FnOnce() -> Result<*mut T, RuntimeError>,
{
    catch_unwind(AssertUnwindSafe(func))
        .ok()
        .and_then(Result::ok)
        .unwrap_or(core::ptr::null_mut())
}

fn ffi_void<F>(func: F)
where
    F: FnOnce() -> Result<(), RuntimeError>,
{
    let _ = catch_unwind(AssertUnwindSafe(func));
}

fn display_value(writer: &mut dyn Write, value: Value) -> Result<(), RuntimeError> {
    write_value(writer, value, false)
}

fn write_value(writer: &mut dyn Write, value: Value, quoted_strings: bool) -> Result<(), RuntimeError> {
    if let Some(fixnum) = value.decode_fixnum() {
        write!(writer, "{fixnum}").map_err(|error| RuntimeError::io_like(error))?;
        return Ok(());
    }

    match value.decode_immediate() {
        Some(crate::value::Immediate::Bool(true)) => {
            writer.write_all(b"#t").map_err(|error| RuntimeError::io_like(error))?;
            return Ok(());
        }
        Some(crate::value::Immediate::Bool(false)) => {
            writer.write_all(b"#f").map_err(|error| RuntimeError::io_like(error))?;
            return Ok(());
        }
        Some(crate::value::Immediate::Char(value)) => {
            match value {
                ' ' => writer.write_all(b"#\\space"),
                '\n' => writer.write_all(b"#\\newline"),
                value => {
                    let mut buffer = [0u8; 4];
                    let encoded = value.encode_utf8(&mut buffer);
                    writer.write_all(b"#\\").and_then(|_| writer.write_all(encoded.as_bytes()))
                }
            }
            .map_err(|error| RuntimeError::io_like(error))?;
            return Ok(());
        }
        Some(crate::value::Immediate::EmptyList) => {
            writer.write_all(b"()").map_err(|error| RuntimeError::io_like(error))?;
            return Ok(());
        }
        Some(crate::value::Immediate::Unspecified) => {
            writer
                .write_all(b"#<unspecified>")
                .map_err(|error| RuntimeError::io_like(error))?;
            return Ok(());
        }
        None => {}
    }

    let object = value
        .to_object_reference()
        .ok_or(RuntimeError::InvalidObjectKind)?;
    match object_kind(value)? {
        crate::layout::HEADER_TAG_PAIR => {
            let pair = unsafe { &*object.to_raw_address().to_ptr::<PairObject>() };
            writer.write_all(b"(").map_err(|error| RuntimeError::io_like(error))?;
            display_pair(writer, pair)?;
            writer.write_all(b")").map_err(|error| RuntimeError::io_like(error))?;
        }
        crate::layout::HEADER_TAG_STRING => {
            let string = unsafe { &*object.to_raw_address().to_ptr::<StringObject>() };
            let bytes = unsafe { core::slice::from_raw_parts(string.bytes_ptr(), string.length) };
            if quoted_strings {
                writer.write_all(b"\"").map_err(|error| RuntimeError::io_like(error))?;
            }
            writer.write_all(bytes).map_err(|error| RuntimeError::io_like(error))?;
            if quoted_strings {
                writer.write_all(b"\"").map_err(|error| RuntimeError::io_like(error))?;
            }
        }
        crate::layout::HEADER_TAG_SYMBOL => {
            let symbol = unsafe { &*object.to_raw_address().to_ptr::<SymbolObject>() };
            let bytes = unsafe { core::slice::from_raw_parts(symbol.bytes_ptr(), symbol.length) };
            writer.write_all(bytes).map_err(|error| RuntimeError::io_like(error))?;
        }
        crate::layout::HEADER_TAG_VECTOR => {
            let vector = unsafe { &*object.to_raw_address().to_ptr::<VectorObject>() };
            writer.write_all(b"#(").map_err(|error| RuntimeError::io_like(error))?;
            for index in 0..vector.length {
                if index != 0 {
                    writer.write_all(b" ").map_err(|error| RuntimeError::io_like(error))?;
                }
                let element = Value::from_bits(unsafe { *vector.elements_ptr().add(index) });
                write_value(writer, element, quoted_strings)?;
            }
            writer.write_all(b")").map_err(|error| RuntimeError::io_like(error))?;
        }
        crate::layout::HEADER_TAG_BOX => {
            let boxed = unsafe { &*object.to_raw_address().to_ptr::<crate::object::BoxObject>() };
            writer.write_all(b"#&").map_err(|error| RuntimeError::io_like(error))?;
            write_value(writer, boxed.value(), quoted_strings)?;
        }
        crate::layout::HEADER_TAG_CLOSURE => {
            writer
                .write_all(b"#<closure>")
                .map_err(|error| RuntimeError::io_like(error))?;
        }
        _ => {
            writer
                .write_all(b"#<object>")
                .map_err(|error| RuntimeError::io_like(error))?;
        }
    }

    Ok(())
}

fn display_pair(writer: &mut dyn Write, pair: &PairObject) -> Result<(), RuntimeError> {
    write_value(writer, pair.car(), false)?;
    match pair.cdr().decode_immediate() {
        Some(crate::value::Immediate::EmptyList) => Ok(()),
        _ => {
            if let Some(object) = pair.cdr().to_object_reference()
                && unsafe { (*object.to_raw_address().to_ptr::<crate::layout::ObjectHeader>()).kind }
                    == crate::layout::HEADER_TAG_PAIR
            {
                let cdr_pair = unsafe { &*object.to_raw_address().to_ptr::<PairObject>() };
                writer.write_all(b" ").map_err(|error| RuntimeError::io_like(error))?;
                return display_pair(writer, cdr_pair);
            }
            writer.write_all(b" . ").map_err(|error| RuntimeError::io_like(error))?;
            write_value(writer, pair.cdr(), false)
        }
    }
}

fn write_pair(writer: &mut dyn Write, pair: &PairObject) -> Result<(), RuntimeError> {
    write_value(writer, pair.car(), true)?;
    match pair.cdr().decode_immediate() {
        Some(crate::value::Immediate::EmptyList) => Ok(()),
        _ => {
            if let Some(object) = pair.cdr().to_object_reference()
                && unsafe { (*object.to_raw_address().to_ptr::<crate::layout::ObjectHeader>()).kind }
                    == crate::layout::HEADER_TAG_PAIR
            {
                let cdr_pair = unsafe { &*object.to_raw_address().to_ptr::<PairObject>() };
                writer.write_all(b" ").map_err(|error| RuntimeError::io_like(error))?;
                return write_pair(writer, cdr_pair);
            }
            writer.write_all(b" . ").map_err(|error| RuntimeError::io_like(error))?;
            write_value(writer, pair.cdr(), true)
        }
    }
}

fn write_scheme_value(writer: &mut dyn Write, value: Value) -> Result<(), RuntimeError> {
    if let Some(fixnum) = value.decode_fixnum() {
        write!(writer, "{fixnum}").map_err(|error| RuntimeError::io_like(error))?;
        return Ok(());
    }

    match value.decode_immediate() {
        Some(crate::value::Immediate::Bool(true)) => {
            writer.write_all(b"#t").map_err(|error| RuntimeError::io_like(error))?;
            return Ok(());
        }
        Some(crate::value::Immediate::Bool(false)) => {
            writer.write_all(b"#f").map_err(|error| RuntimeError::io_like(error))?;
            return Ok(());
        }
        Some(crate::value::Immediate::Char(value)) => {
            match value {
                ' ' => writer.write_all(b"#\\space"),
                '\n' => writer.write_all(b"#\\newline"),
                value => {
                    let mut buffer = [0u8; 4];
                    let encoded = value.encode_utf8(&mut buffer);
                    writer.write_all(b"#\\").and_then(|_| writer.write_all(encoded.as_bytes()))
                }
            }
            .map_err(|error| RuntimeError::io_like(error))?;
            return Ok(());
        }
        Some(crate::value::Immediate::EmptyList) => {
            writer.write_all(b"()").map_err(|error| RuntimeError::io_like(error))?;
            return Ok(());
        }
        Some(crate::value::Immediate::Unspecified) => {
            writer
                .write_all(b"#<unspecified>")
                .map_err(|error| RuntimeError::io_like(error))?;
            return Ok(());
        }
        None => {}
    }

    let object = value
        .to_object_reference()
        .ok_or(RuntimeError::InvalidObjectKind)?;
    match object_kind(value)? {
        crate::layout::HEADER_TAG_PAIR => {
            let pair = unsafe { &*object.to_raw_address().to_ptr::<PairObject>() };
            writer.write_all(b"(").map_err(|error| RuntimeError::io_like(error))?;
            write_pair(writer, pair)?;
            writer.write_all(b")").map_err(|error| RuntimeError::io_like(error))?;
        }
        _ => write_value(writer, value, true)?,
    }
    Ok(())
}

fn is_proper_list(mut value: Value) -> bool {
    loop {
        match value.decode_immediate() {
            Some(crate::value::Immediate::EmptyList) => return true,
            Some(_) => return false,
            None => {}
        }

        let Some(object) = value.to_object_reference() else {
            return false;
        };
        if unsafe { (*object.to_raw_address().to_ptr::<crate::layout::ObjectHeader>()).kind }
            != crate::layout::HEADER_TAG_PAIR
        {
            return false;
        }
        value = unsafe { (*object.to_raw_address().to_ptr::<PairObject>()).cdr() };
    }
}

fn proper_list_length(mut value: Value) -> Result<usize, RuntimeError> {
    let mut length = 0usize;
    loop {
        match value.decode_immediate() {
            Some(crate::value::Immediate::EmptyList) => return Ok(length),
            Some(_) => return Err(RuntimeError::InvalidObjectKind),
            None => {}
        }

        let object = value
            .to_object_reference()
            .ok_or(RuntimeError::InvalidObjectKind)?;
        if unsafe { (*object.to_raw_address().to_ptr::<crate::layout::ObjectHeader>()).kind }
            != crate::layout::HEADER_TAG_PAIR
        {
            return Err(RuntimeError::InvalidObjectKind);
        }
        value = unsafe { (*object.to_raw_address().to_ptr::<PairObject>()).cdr() };
        length = length.saturating_add(1);
    }
}

fn list_tail_value(mut value: Value, index: usize) -> Result<Value, RuntimeError> {
    for _ in 0..index {
        let object = value
            .to_object_reference()
            .ok_or(RuntimeError::InvalidObjectKind)?;
        if unsafe { (*object.to_raw_address().to_ptr::<crate::layout::ObjectHeader>()).kind }
            != crate::layout::HEADER_TAG_PAIR
        {
            return Err(RuntimeError::InvalidObjectKind);
        }
        value = unsafe { (*object.to_raw_address().to_ptr::<PairObject>()).cdr() };
    }
    Ok(value)
}

fn list_ref_value(value: Value, index: usize) -> Result<Value, RuntimeError> {
    let tail = list_tail_value(value, index)?;
    let object = tail
        .to_object_reference()
        .ok_or(RuntimeError::InvalidObjectKind)?;
    if unsafe { (*object.to_raw_address().to_ptr::<crate::layout::ObjectHeader>()).kind }
        != crate::layout::HEADER_TAG_PAIR
    {
        return Err(RuntimeError::InvalidObjectKind);
    }
    Ok(unsafe { (*object.to_raw_address().to_ptr::<PairObject>()).car() })
}

fn append_two_lists(mut left: Value, right: Value) -> Result<Value, RuntimeError> {
    match left.decode_immediate() {
        Some(crate::value::Immediate::EmptyList) => return Ok(right),
        Some(_) => return Err(RuntimeError::InvalidObjectKind),
        None => {}
    }

    let mut elements = Vec::new();
    loop {
        match left.decode_immediate() {
            Some(crate::value::Immediate::EmptyList) => break,
            Some(_) => return Err(RuntimeError::InvalidObjectKind),
            None => {}
        }

        let object = left
            .to_object_reference()
            .ok_or(RuntimeError::InvalidObjectKind)?;
        if unsafe { (*object.to_raw_address().to_ptr::<crate::layout::ObjectHeader>()).kind }
            != crate::layout::HEADER_TAG_PAIR
        {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let pair = unsafe { &*object.to_raw_address().to_ptr::<PairObject>() };
        elements.push(pair.car());
        left = pair.cdr();
    }

    let mut result = right;
    for element in elements.into_iter().rev() {
        result = Value::from_object_reference(alloc_pair_checked(element, result)?);
    }
    Ok(result)
}

#[unsafe(no_mangle)]
pub extern "C" fn rt_mmtk_init(heap_size_bytes: usize, worker_count: usize) -> bool {
    ffi_bool(|| {
        initialize_runtime(heap_size_bytes.max(1024 * 1024), worker_count.max(1));
        Ok(true)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn rt_bind_thread() -> *mut core::ffi::c_void {
    ffi_ptr(|| Ok(bind_thread_handle()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rt_unbind_thread(thread: *mut core::ffi::c_void) {
    ffi_void(|| {
        unsafe { unbind_thread_handle(thread) };
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn rt_alloc_slow(size: usize, align: usize, kind: u16) -> *mut core::ffi::c_void {
    ffi_ptr(|| {
        Ok(alloc_raw_checked(size, align, kind)?
            .to_raw_address()
            .to_mut_ptr::<core::ffi::c_void>())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn rt_gc_poll() {
    ffi_void(gc_poll_current_checked);
}

#[unsafe(no_mangle)]
pub extern "C" fn gc_safepoint_poll() {
    rt_gc_poll();
}

#[unsafe(no_mangle)]
pub extern "C" fn rt_object_write_post(
    src: *mut core::ffi::c_void,
    slot: *mut usize,
    target: usize,
) {
    ffi_void(|| {
        let Some(src) = mmtk::util::ObjectReference::from_raw_address(
            mmtk::util::Address::from_mut_ptr(src),
        ) else {
            return Ok(());
        };
        if slot.is_null() {
            return Err(RuntimeError::NullSlot);
        }
        object_write_post_checked(src, slot, Value::from_bits(target))?;
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn rt_root_slot_push(slot: *mut usize) {
    ffi_void(|| push_root_checked(current_thread(), slot));
}

#[unsafe(no_mangle)]
pub extern "C" fn rt_root_slot_pop() {
    ffi_void(|| pop_root_checked(current_thread()));
}

#[unsafe(no_mangle)]
pub extern "C" fn rt_register_global_root(slot: *mut usize) {
    ffi_void(|| register_global_root_checked(slot));
}

#[unsafe(no_mangle)]
pub extern "C" fn rt_run_mutator_stress(thread_count: usize, iterations: usize) {
    ffi_void(|| run_mutator_stress_checked(thread_count, iterations));
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_make_fixnum(value: i64) -> usize {
    ffi_word(|| {
        Value::encode_fixnum(value)
            .ok_or(RuntimeError::FixnumOutOfRange)
            .map(Value::bits)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_make_bool(value: bool) -> usize {
    ffi_word(|| Ok(Value::encode_bool(value).bits()))
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_empty_list() -> usize {
    ffi_word(|| Ok(Value::empty_list().bits()))
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_unspecified() -> usize {
    ffi_word(|| Ok(Value::unspecified().bits()))
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_gc_stress(iterations: usize) -> usize {
    ffi_word(|| {
        gc_stress_checked(iterations)?;
        Ok(Value::unspecified().bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_display(value: usize) -> usize {
    ffi_word(|| {
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        display_value(&mut handle, Value::from_bits(value))?;
        handle.flush().map_err(|error| RuntimeError::io_like(error))?;
        Ok(Value::unspecified().bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_write(value: usize) -> usize {
    ffi_word(|| {
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        write_scheme_value(&mut handle, Value::from_bits(value))?;
        handle.flush().map_err(|error| RuntimeError::io_like(error))?;
        Ok(Value::unspecified().bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_newline() -> usize {
    ffi_word(|| {
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        handle.write_all(b"\n").map_err(|error| RuntimeError::io_like(error))?;
        handle.flush().map_err(|error| RuntimeError::io_like(error))?;
        Ok(Value::unspecified().bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_alloc_pair(car: usize, cdr: usize) -> usize {
    ffi_word(|| {
        Ok(Value::from_object_reference(alloc_pair_checked(
            Value::from_bits(car),
            Value::from_bits(cdr),
        )?)
        .bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_alloc_box(value: usize) -> usize {
    ffi_word(|| Ok(Value::from_object_reference(alloc_box_checked(Value::from_bits(value))?).bits()))
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_alloc_box_gc(value: usize) -> *mut crate::object::BoxObject {
    ffi_ptr(|| {
        Ok(alloc_box_checked(Value::from_bits(value))?
            .to_raw_address()
            .to_mut_ptr())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_box_set_gc(
    boxed: *mut crate::object::BoxObject,
    value: usize,
) -> usize {
    ffi_word(|| {
        if boxed.is_null() {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let object = mmtk::util::ObjectReference::from_raw_address(
            mmtk::util::Address::from_mut_ptr(boxed.cast::<core::ffi::c_void>()),
        )
        .ok_or(RuntimeError::InvalidObjectKind)?;
        object_write_post_checked(
            object,
            unsafe { core::ptr::addr_of_mut!((*boxed).value) },
            Value::from_bits(value),
        )?;
        Ok(Value::unspecified().bits())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_alloc_closure(code_ptr: usize, env_values: *const usize, env_len: usize) -> usize {
    ffi_word(|| {
        if env_len != 0 && env_values.is_null() {
            return Err(RuntimeError::NullSlot);
        }
        let slice = if env_len == 0 {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(env_values, env_len) }
        };
        let env = slice.iter().copied().map(Value::from_bits).collect::<Vec<_>>();
        Ok(Value::from_object_reference(alloc_closure_checked(code_ptr, &env)?).bits())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_alloc_closure_gc(
    code_ptr: usize,
    env_values: *const usize,
    env_len: usize,
) -> *mut ClosureObject {
    ffi_ptr(|| {
        if env_len != 0 && env_values.is_null() {
            return Err(RuntimeError::NullSlot);
        }
        let slice = if env_len == 0 {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(env_values, env_len) }
        };
        let env = slice.iter().copied().map(Value::from_bits).collect::<Vec<_>>();
        Ok(alloc_closure_checked(code_ptr, &env)?.to_raw_address().to_mut_ptr())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_closure_code_ptr_gc(closure: *mut ClosureObject) -> usize {
    ffi_word(|| {
        if closure.is_null() {
            return Err(RuntimeError::InvalidObjectKind);
        }
        Ok(unsafe { (*closure).code_ptr })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_closure_env_ref_gc(closure: *mut ClosureObject, index: usize) -> usize {
    ffi_word(|| {
        if closure.is_null() {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let closure = unsafe { &*closure };
        if index >= closure.env_len {
            return Err(RuntimeError::IndexOutOfBounds);
        }
        Ok(unsafe { *closure.env_ptr().add(index) })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_closure_env_set_gc(
    closure: *mut ClosureObject,
    index: usize,
    value: usize,
) -> usize {
    ffi_word(|| {
        if closure.is_null() {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let closure_ref = mmtk::util::ObjectReference::from_raw_address(
            mmtk::util::Address::from_mut_ptr(closure.cast::<core::ffi::c_void>()),
        )
        .ok_or(RuntimeError::InvalidObjectKind)?;
        let closure = unsafe { &mut *closure };
        if index >= closure.env_len {
            return Err(RuntimeError::IndexOutOfBounds);
        }
        object_write_post_checked(
            closure_ref,
            unsafe { closure.env_mut_ptr().add(index) },
            Value::from_bits(value),
        )?;
        Ok(Value::unspecified().bits())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_alloc_string(bytes: *const u8, len: usize) -> usize {
    ffi_word(|| {
        if bytes.is_null() {
            return Err(RuntimeError::NullSlot);
        }
        let slice = unsafe { core::slice::from_raw_parts(bytes, len) };
        Ok(Value::from_object_reference(alloc_string_checked(slice)?).bits())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_alloc_string_gc(bytes: *const u8, len: usize) -> *mut StringObject {
    ffi_ptr(|| {
        if bytes.is_null() && len != 0 {
            return Err(RuntimeError::NullSlot);
        }
        let slice = if len == 0 {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(bytes, len) }
        };
        Ok(alloc_string_checked(slice)?.to_raw_address().to_mut_ptr())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_alloc_symbol(bytes: *const u8, len: usize) -> usize {
    ffi_word(|| {
        if bytes.is_null() && len != 0 {
            return Err(RuntimeError::NullSlot);
        }
        let slice = if len == 0 {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(bytes, len) }
        };
        Ok(Value::from_object_reference(alloc_symbol_checked(slice)?).bits())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_alloc_symbol_gc(bytes: *const u8, len: usize) -> *mut SymbolObject {
    ffi_ptr(|| {
        if bytes.is_null() && len != 0 {
            return Err(RuntimeError::NullSlot);
        }
        let slice = if len == 0 {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(bytes, len) }
        };
        Ok(alloc_symbol_checked(slice)?.to_raw_address().to_mut_ptr())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_is_string(value: usize) -> bool {
    ffi_bool(|| Ok(object_kind(Value::from_bits(value)).ok() == Some(crate::layout::HEADER_TAG_STRING)))
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_is_symbol(value: usize) -> bool {
    ffi_bool(|| Ok(object_kind(Value::from_bits(value)).ok() == Some(crate::layout::HEADER_TAG_SYMBOL)))
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_string_length(value: usize) -> usize {
    ffi_word(|| {
        let object = Value::from_bits(value)
            .to_object_reference()
            .ok_or(RuntimeError::InvalidObjectKind)?;
        if object_kind(Value::from_bits(value))? != crate::layout::HEADER_TAG_STRING {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let string = unsafe { &*object.to_raw_address().to_ptr::<StringObject>() };
        let length = i64::try_from(string.length).map_err(|_| RuntimeError::FixnumOutOfRange)?;
        Value::encode_fixnum(length)
            .ok_or(RuntimeError::FixnumOutOfRange)
            .map(Value::bits)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_string_length_gc(string: *mut StringObject) -> usize {
    ffi_word(|| {
        if string.is_null() {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let length = i64::try_from(unsafe { (*string).length })
            .map_err(|_| RuntimeError::FixnumOutOfRange)?;
        Value::encode_fixnum(length)
            .ok_or(RuntimeError::FixnumOutOfRange)
            .map(Value::bits)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_string_ref(value: usize, index: usize) -> usize {
    ffi_word(|| {
        let object = Value::from_bits(value)
            .to_object_reference()
            .ok_or(RuntimeError::InvalidObjectKind)?;
        if object_kind(Value::from_bits(value))? != crate::layout::HEADER_TAG_STRING {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let string = unsafe { &*object.to_raw_address().to_ptr::<StringObject>() };
        if index >= string.length {
            return Err(RuntimeError::IndexOutOfBounds);
        }
        let byte = unsafe { *string.bytes_ptr().add(index) };
        Value::encode_fixnum(byte as i64)
            .ok_or(RuntimeError::FixnumOutOfRange)
            .map(Value::bits)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_string_ref_gc(string: *mut StringObject, index: usize) -> usize {
    ffi_word(|| {
        if string.is_null() {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let string = unsafe { &*string };
        if index >= string.length {
            return Err(RuntimeError::IndexOutOfBounds);
        }
        let byte = unsafe { *string.bytes_ptr().add(index) };
        Value::encode_fixnum(byte as i64)
            .ok_or(RuntimeError::FixnumOutOfRange)
            .map(Value::bits)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_alloc_vector(values: *const usize, len: usize) -> usize {
    ffi_word(|| {
        if len != 0 && values.is_null() {
            return Err(RuntimeError::NullSlot);
        }
        let slice = if len == 0 {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(values, len) }
        };
        let elements = slice.iter().copied().map(Value::from_bits).collect::<Vec<_>>();
        Ok(Value::from_object_reference(alloc_vector_checked(&elements)?).bits())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_alloc_vector_gc(values: *const usize, len: usize) -> *mut VectorObject {
    ffi_ptr(|| {
        if len != 0 && values.is_null() {
            return Err(RuntimeError::NullSlot);
        }
        let slice = if len == 0 {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(values, len) }
        };
        let elements = slice.iter().copied().map(Value::from_bits).collect::<Vec<_>>();
        Ok(alloc_vector_checked(&elements)?.to_raw_address().to_mut_ptr())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_is_vector(value: usize) -> bool {
    ffi_bool(|| Ok(object_kind(Value::from_bits(value)).ok() == Some(crate::layout::HEADER_TAG_VECTOR)))
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_vector_length(value: usize) -> usize {
    ffi_word(|| {
        let object = Value::from_bits(value)
            .to_object_reference()
            .ok_or(RuntimeError::InvalidObjectKind)?;
        if object_kind(Value::from_bits(value))? != crate::layout::HEADER_TAG_VECTOR {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let vector = unsafe { &*object.to_raw_address().to_ptr::<VectorObject>() };
        let length = i64::try_from(vector.length).map_err(|_| RuntimeError::FixnumOutOfRange)?;
        Value::encode_fixnum(length)
            .ok_or(RuntimeError::FixnumOutOfRange)
            .map(Value::bits)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_vector_length_gc(vector: *mut VectorObject) -> usize {
    ffi_word(|| {
        if vector.is_null() {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let length = i64::try_from(unsafe { (*vector).length })
            .map_err(|_| RuntimeError::FixnumOutOfRange)?;
        Value::encode_fixnum(length)
            .ok_or(RuntimeError::FixnumOutOfRange)
            .map(Value::bits)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_vector_ref(value: usize, index: usize) -> usize {
    ffi_word(|| {
        let object = Value::from_bits(value)
            .to_object_reference()
            .ok_or(RuntimeError::InvalidObjectKind)?;
        if object_kind(Value::from_bits(value))? != crate::layout::HEADER_TAG_VECTOR {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let vector = unsafe { &*object.to_raw_address().to_ptr::<VectorObject>() };
        if index >= vector.length {
            return Err(RuntimeError::IndexOutOfBounds);
        }
        Ok(Value::from_bits(unsafe { *vector.elements_ptr().add(index) }).bits())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_vector_ref_gc(vector: *mut VectorObject, index: usize) -> usize {
    ffi_word(|| {
        if vector.is_null() {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let vector = unsafe { &*vector };
        if index >= vector.length {
            return Err(RuntimeError::IndexOutOfBounds);
        }
        Ok(Value::from_bits(unsafe { *vector.elements_ptr().add(index) }).bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_vector_set(value: usize, index: usize, element: usize) -> usize {
    ffi_word(|| {
        let object = Value::from_bits(value)
            .to_object_reference()
            .ok_or(RuntimeError::InvalidObjectKind)?;
        if object_kind(Value::from_bits(value))? != crate::layout::HEADER_TAG_VECTOR {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let vector = unsafe { &mut *object.to_raw_address().to_mut_ptr::<VectorObject>() };
        if index >= vector.length {
            return Err(RuntimeError::IndexOutOfBounds);
        }
        object_write_post_checked(object, unsafe { vector.elements_mut_ptr().add(index) }, Value::from_bits(element))?;
        Ok(Value::unspecified().bits())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_vector_set_gc(
    vector: *mut VectorObject,
    index: usize,
    element: usize,
) -> usize {
    ffi_word(|| {
        if vector.is_null() {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let object = mmtk::util::ObjectReference::from_raw_address(
            mmtk::util::Address::from_mut_ptr(vector),
        )
        .ok_or(RuntimeError::InvalidObjectKind)?;
        let vector = unsafe { &mut *vector };
        if index >= vector.length {
            return Err(RuntimeError::IndexOutOfBounds);
        }
        object_write_post_checked(
            object,
            unsafe { vector.elements_mut_ptr().add(index) },
            Value::from_bits(element),
        )?;
        Ok(Value::unspecified().bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_alloc_pair_gc(car: usize, cdr: usize) -> *mut PairObject {
    ffi_ptr(|| {
        Ok(alloc_pair_checked(Value::from_bits(car), Value::from_bits(cdr))?
            .to_raw_address()
            .to_mut_ptr())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_pair_car(pair: usize) -> usize {
    ffi_word(|| {
        let pair = Value::from_bits(pair)
            .to_object_reference()
            .ok_or(RuntimeError::InvalidThread)?
            .to_raw_address()
            .to_ptr::<PairObject>();
        Ok(unsafe { (*pair).car() }.bits())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_pair_cdr(pair: usize) -> usize {
    ffi_word(|| {
        let pair = Value::from_bits(pair)
            .to_object_reference()
            .ok_or(RuntimeError::InvalidThread)?
            .to_raw_address()
            .to_ptr::<PairObject>();
        Ok(unsafe { (*pair).cdr() }.bits())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_pair_car_gc(pair: *mut PairObject) -> usize {
    ffi_word(|| {
        if pair.is_null() {
            return Err(RuntimeError::InvalidThread);
        }
        Ok(unsafe { (*pair).car() }.bits())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_pair_cdr_gc(pair: *mut PairObject) -> usize {
    ffi_word(|| {
        if pair.is_null() {
            return Err(RuntimeError::InvalidThread);
        }
        Ok(unsafe { (*pair).cdr() }.bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_is_pair(value: usize) -> bool {
    ffi_bool(|| Ok(object_kind(Value::from_bits(value)).ok() == Some(crate::layout::HEADER_TAG_PAIR)))
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_is_list(value: usize) -> bool {
    ffi_bool(|| Ok(is_proper_list(Value::from_bits(value))))
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_list_length(value: usize) -> usize {
    ffi_word(|| {
        let length = proper_list_length(Value::from_bits(value))?;
        Value::encode_fixnum(length as i64)
            .ok_or(RuntimeError::FixnumOutOfRange)
            .map(Value::bits)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_list_tail(value: usize, index: usize) -> usize {
    ffi_word(|| Ok(list_tail_value(Value::from_bits(value), index)?.bits()))
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_list_ref(value: usize, index: usize) -> usize {
    ffi_word(|| Ok(list_ref_value(Value::from_bits(value), index)?.bits()))
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_append(left: usize, right: usize) -> usize {
    ffi_word(|| Ok(append_two_lists(Value::from_bits(left), Value::from_bits(right))?.bits()))
}

#[cfg(test)]
mod tests {
    use super::{
        mlisp_alloc_pair, mlisp_alloc_pair_gc, mlisp_alloc_string, mlisp_alloc_string_gc,
        mlisp_alloc_vector, mlisp_alloc_vector_gc, mlisp_append, mlisp_is_list, mlisp_is_pair,
        mlisp_is_string, mlisp_is_symbol, mlisp_is_vector, mlisp_list_length, mlisp_list_ref,
        mlisp_list_tail, mlisp_make_fixnum, mlisp_pair_car, mlisp_pair_car_gc, mlisp_pair_cdr,
        mlisp_pair_cdr_gc, mlisp_string_length, mlisp_string_length_gc, mlisp_string_ref,
        mlisp_string_ref_gc, mlisp_vector_length, mlisp_vector_length_gc, mlisp_vector_ref,
        mlisp_vector_ref_gc, mlisp_vector_set, mlisp_vector_set_gc, rt_bind_thread, rt_gc_poll,
        rt_mmtk_init, rt_run_mutator_stress, rt_unbind_thread,
    };
    use crate::value::Value;

    #[test]
    fn allocates_pair_through_mmtk_runtime() {
        assert!(rt_mmtk_init(8 * 1024 * 1024, 1));
        let thread = rt_bind_thread();
        let pair = mlisp_alloc_pair(mlisp_make_fixnum(7), mlisp_make_fixnum(9));
        assert!(mlisp_is_pair(pair));
        let car = unsafe { mlisp_pair_car(pair) };
        let cdr = unsafe { mlisp_pair_cdr(pair) };
        assert_eq!(Value::from_bits(car).decode_fixnum(), Some(7));
        assert_eq!(Value::from_bits(cdr).decode_fixnum(), Some(9));
        rt_gc_poll();
        unsafe { rt_unbind_thread(thread) };
    }

    #[test]
    fn recognizes_proper_lists_and_computes_length() {
        assert!(rt_mmtk_init(8 * 1024 * 1024, 1));
        let thread = rt_bind_thread();
        let empty = Value::empty_list().bits();
        let tail = mlisp_alloc_pair(mlisp_make_fixnum(2), empty);
        let list = mlisp_alloc_pair(mlisp_make_fixnum(1), tail);
        let dotted = mlisp_alloc_pair(mlisp_make_fixnum(1), mlisp_make_fixnum(2));
        assert!(mlisp_is_list(empty));
        assert!(mlisp_is_list(list));
        assert!(!mlisp_is_list(dotted));
        assert_eq!(Value::from_bits(mlisp_list_length(list)).decode_fixnum(), Some(2));
        assert_eq!(Value::from_bits(mlisp_list_ref(list, 1)).decode_fixnum(), Some(2));
        assert!(mlisp_is_list(mlisp_list_tail(list, 1)));
        assert_eq!(
            mlisp_list_length(dotted),
            Value::unspecified().bits()
        );
        unsafe { rt_unbind_thread(thread) };
    }

    #[test]
    fn allocates_raw_pair_pointer_through_mmtk_runtime() {
        assert!(rt_mmtk_init(8 * 1024 * 1024, 1));
        let thread = rt_bind_thread();
        let pair = mlisp_alloc_pair_gc(mlisp_make_fixnum(1), mlisp_make_fixnum(2));
        let car = unsafe { mlisp_pair_car_gc(pair) };
        let cdr = unsafe { mlisp_pair_cdr_gc(pair) };
        assert_eq!(Value::from_bits(car).decode_fixnum(), Some(1));
        assert_eq!(Value::from_bits(cdr).decode_fixnum(), Some(2));
        unsafe { rt_unbind_thread(thread) };
    }

    #[test]
    fn runs_multithreaded_mutator_stress() {
        assert!(rt_mmtk_init(8 * 1024 * 1024, 2));
        rt_run_mutator_stress(2, 32);
    }

    #[test]
    fn allocates_string_through_mmtk_runtime() {
        assert!(rt_mmtk_init(8 * 1024 * 1024, 1));
        let thread = rt_bind_thread();
        let value = unsafe { mlisp_alloc_string(b"hello".as_ptr(), 5) };
        assert!(mlisp_is_string(value));
        assert_eq!(Value::from_bits(mlisp_string_length(value)).decode_fixnum(), Some(5));
        assert_eq!(
            Value::from_bits(mlisp_string_ref(value, 1)).decode_fixnum(),
            Some(b'e' as i64)
        );
        rt_gc_poll();
        assert_eq!(
            Value::from_bits(mlisp_string_ref(value, 4)).decode_fixnum(),
            Some(b'o' as i64)
        );
        unsafe { rt_unbind_thread(thread) };
    }

    #[test]
    fn allocates_raw_string_pointer_through_mmtk_runtime() {
        assert!(rt_mmtk_init(8 * 1024 * 1024, 1));
        let thread = rt_bind_thread();
        let string = unsafe { mlisp_alloc_string_gc(b"hi".as_ptr(), 2) };
        assert_eq!(Value::from_bits(unsafe { mlisp_string_length_gc(string) }).decode_fixnum(), Some(2));
        assert_eq!(
            Value::from_bits(unsafe { mlisp_string_ref_gc(string, 1) }).decode_fixnum(),
            Some(b'i' as i64)
        );
        unsafe { rt_unbind_thread(thread) };
    }

    #[test]
    fn recognizes_symbols_through_runtime() {
        assert!(rt_mmtk_init(8 * 1024 * 1024, 1));
        let thread = rt_bind_thread();
        let value = unsafe { super::mlisp_alloc_symbol(b"hello".as_ptr(), 5) };
        assert!(mlisp_is_symbol(value));
        assert!(!mlisp_is_symbol(mlisp_make_fixnum(1)));
        unsafe { rt_unbind_thread(thread) };
    }

    #[test]
    fn appends_lists_through_runtime() {
        assert!(rt_mmtk_init(8 * 1024 * 1024, 1));
        let thread = rt_bind_thread();
        let empty = Value::empty_list().bits();
        let left_tail = mlisp_alloc_pair(mlisp_make_fixnum(2), empty);
        let left = mlisp_alloc_pair(mlisp_make_fixnum(1), left_tail);
        let right = mlisp_alloc_pair(mlisp_make_fixnum(3), empty);
        let appended = mlisp_append(left, right);
        assert!(mlisp_is_list(appended));
        assert_eq!(Value::from_bits(mlisp_list_length(appended)).decode_fixnum(), Some(3));
        assert_eq!(Value::from_bits(mlisp_list_ref(appended, 2)).decode_fixnum(), Some(3));
        unsafe { rt_unbind_thread(thread) };
    }

    #[test]
    fn allocates_and_mutates_vector_through_mmtk_runtime() {
        assert!(rt_mmtk_init(8 * 1024 * 1024, 1));
        let thread = rt_bind_thread();
        let string = unsafe { mlisp_alloc_string(b"ok".as_ptr(), 2) };
        let elements = [mlisp_make_fixnum(7), string];
        let vector = unsafe { mlisp_alloc_vector(elements.as_ptr(), elements.len()) };
        assert!(mlisp_is_vector(vector));
        assert_eq!(Value::from_bits(mlisp_vector_length(vector)).decode_fixnum(), Some(2));
        assert_eq!(Value::from_bits(mlisp_vector_ref(vector, 0)).decode_fixnum(), Some(7));
        assert_eq!(mlisp_vector_ref(vector, 1), string);

        let replacement = mlisp_alloc_pair(mlisp_make_fixnum(1), mlisp_make_fixnum(2));
        assert_eq!(mlisp_vector_set(vector, 1, replacement), Value::unspecified().bits());
        rt_gc_poll();
        assert_eq!(mlisp_vector_ref(vector, 1), replacement);
        unsafe { rt_unbind_thread(thread) };
    }

    #[test]
    fn allocates_and_mutates_raw_vector_pointer_through_mmtk_runtime() {
        assert!(rt_mmtk_init(8 * 1024 * 1024, 1));
        let thread = rt_bind_thread();
        let string = unsafe { mlisp_alloc_string(b"gc".as_ptr(), 2) };
        let elements = [mlisp_make_fixnum(4), string];
        let vector = unsafe { mlisp_alloc_vector_gc(elements.as_ptr(), elements.len()) };
        assert_eq!(
            Value::from_bits(unsafe { mlisp_vector_length_gc(vector) }).decode_fixnum(),
            Some(2)
        );
        assert_eq!(
            Value::from_bits(unsafe { mlisp_vector_ref_gc(vector, 0) }).decode_fixnum(),
            Some(4)
        );
        let replacement = mlisp_alloc_pair(mlisp_make_fixnum(8), mlisp_make_fixnum(9));
        assert_eq!(
            unsafe { mlisp_vector_set_gc(vector, 1, replacement) },
            Value::unspecified().bits()
        );
        rt_gc_poll();
        assert_eq!(unsafe { mlisp_vector_ref_gc(vector, 1) }, replacement);
        unsafe { rt_unbind_thread(thread) };
    }

    #[test]
    fn ffi_accessors_fail_closed_for_invalid_inputs() {
        assert_eq!(
            unsafe { mlisp_pair_car(mlisp_make_fixnum(1)) },
            Value::unspecified().bits()
        );
        assert_eq!(unsafe { mlisp_pair_car_gc(core::ptr::null_mut()) }, Value::unspecified().bits());
        unsafe { rt_unbind_thread(core::ptr::null_mut()) };
    }
}
