use crate::error::RuntimeError;
use crate::mmtk::{
    alloc_box_checked, alloc_closure_checked, alloc_pair_checked, alloc_raw_checked,
    alloc_promise_checked, alloc_string_checked, alloc_symbol_checked, alloc_values_checked, alloc_vector_checked, bind_thread_handle,
    current_thread, exception_pending_checked, gc_poll_current_checked, gc_stress_checked,
    initialize_runtime, object_write_post_checked, pop_root_checked, push_root_checked,
    raise_checked, register_global_root_checked, run_mutator_stress_checked,
    take_pending_exception_checked, unbind_thread_handle,
};
use crate::object::{ClosureObject, PairObject, PromiseObject, StringObject, SymbolObject, ValuesObject, VectorObject};
use crate::value::Value;
use std::io::{self, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Mutex, OnceLock};

fn object_kind(value: Value) -> Result<u16, RuntimeError> {
    let object = value
        .to_object_reference()
        .ok_or(RuntimeError::InvalidObjectKind)?;
    Ok(unsafe {
        (*object
            .to_raw_address()
            .to_ptr::<crate::layout::ObjectHeader>())
        .kind
    })
}

struct RootGuard {
    thread: *mut core::ffi::c_void,
    count: usize,
}

static INTERNED_SYMBOLS: OnceLock<Mutex<std::collections::HashMap<Vec<u8>, Box<usize>>>> =
    OnceLock::new();

fn symbol_intern_table() -> &'static Mutex<std::collections::HashMap<Vec<u8>, Box<usize>>> {
    INTERNED_SYMBOLS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

impl RootGuard {
    fn new(thread: *mut core::ffi::c_void) -> Self {
        Self { thread, count: 0 }
    }

    fn push(&mut self, slot: *mut usize) -> Result<(), RuntimeError> {
        push_root_checked(self.thread, slot)?;
        self.count += 1;
        Ok(())
    }
}

impl Drop for RootGuard {
    fn drop(&mut self) {
        while self.count > 0 {
            let _ = pop_root_checked(self.thread);
            self.count -= 1;
        }
    }
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

fn intern_symbol(bytes: &[u8]) -> Result<Value, RuntimeError> {
    let table = symbol_intern_table();
    if let Some(value) = table
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(bytes)
        .map(|slot| Value::from_bits(**slot))
    {
        return Ok(value);
    }

    let object = alloc_symbol_checked(bytes)?;
    let mut rooted_bits = Value::from_object_reference(object).bits();
    let thread = current_thread();
    let mut roots = RootGuard::new(thread);
    roots.push(&mut rooted_bits)?;
    let mut slot = Box::new(rooted_bits);
    register_global_root_checked(slot.as_mut() as *mut usize)?;
    let mut guard = table.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = guard.entry(bytes.to_vec()).or_insert(slot);
    Ok(Value::from_bits(**entry))
}

const BUILTIN_ADD: u16 = 0;
const BUILTIN_SUB: u16 = 1;
const BUILTIN_MUL: u16 = 2;
const BUILTIN_DIV: u16 = 3;
const BUILTIN_NOT: u16 = 4;
const BUILTIN_BOOLEAN_PREDICATE: u16 = 5;
const BUILTIN_ZERO_PREDICATE: u16 = 6;
const BUILTIN_CHAR_PREDICATE: u16 = 7;
const BUILTIN_CHAR_EQ: u16 = 8;
const BUILTIN_CHAR_LT: u16 = 9;
const BUILTIN_CHAR_GT: u16 = 10;
const BUILTIN_CHAR_LE: u16 = 11;
const BUILTIN_CHAR_GE: u16 = 12;
const BUILTIN_CHAR_TO_INTEGER: u16 = 13;
const BUILTIN_INTEGER_TO_CHAR: u16 = 14;
const BUILTIN_SYMBOL_PREDICATE: u16 = 15;
const BUILTIN_SYMBOL_TO_STRING: u16 = 16;
const BUILTIN_STRING_TO_SYMBOL: u16 = 17;
const BUILTIN_PROCEDURE_PREDICATE: u16 = 18;
const BUILTIN_EQ: u16 = 19;
const BUILTIN_EQV: u16 = 20;
const BUILTIN_EQUAL: u16 = 21;
const BUILTIN_LIST: u16 = 22;
const BUILTIN_APPEND: u16 = 23;
const BUILTIN_MEMQ: u16 = 24;
const BUILTIN_MEMV: u16 = 25;
const BUILTIN_MEMBER: u16 = 26;
const BUILTIN_ASSQ: u16 = 27;
const BUILTIN_ASSV: u16 = 28;
const BUILTIN_ASSOC: u16 = 29;
const BUILTIN_LIST_COPY: u16 = 30;
const BUILTIN_REVERSE: u16 = 31;
const BUILTIN_CONS: u16 = 32;
const BUILTIN_CAR: u16 = 33;
const BUILTIN_CDR: u16 = 34;
const BUILTIN_SET_CAR: u16 = 35;
const BUILTIN_SET_CDR: u16 = 36;
const BUILTIN_PAIR_PREDICATE: u16 = 37;
const BUILTIN_LIST_PREDICATE: u16 = 38;
const BUILTIN_LENGTH: u16 = 39;
const BUILTIN_LIST_TAIL: u16 = 40;
const BUILTIN_LIST_REF: u16 = 41;
const BUILTIN_NULL_PREDICATE: u16 = 42;
const BUILTIN_STRING_PREDICATE: u16 = 43;
const BUILTIN_STRING_LENGTH: u16 = 44;
const BUILTIN_STRING_REF: u16 = 45;
const BUILTIN_DISPLAY: u16 = 46;
const BUILTIN_WRITE: u16 = 47;
const BUILTIN_NEWLINE: u16 = 48;
const BUILTIN_GC_STRESS: u16 = 49;
const BUILTIN_VECTOR: u16 = 50;
const BUILTIN_VECTOR_PREDICATE: u16 = 51;
const BUILTIN_VECTOR_LENGTH: u16 = 52;
const BUILTIN_VECTOR_REF: u16 = 53;
const BUILTIN_VECTOR_SET: u16 = 54;
const BUILTIN_RAISE: u16 = 55;
const BUILTIN_ERROR: u16 = 56;

fn expect_arity(args: &[Value], expected: usize) -> Result<(), RuntimeError> {
    if args.len() == expected {
        Ok(())
    } else {
        Err(RuntimeError::InvalidObjectKind)
    }
}

fn expect_fixnum(value: Value) -> Result<i64, RuntimeError> {
    value.decode_fixnum().ok_or(RuntimeError::InvalidObjectKind)
}

fn expect_char(value: Value) -> Result<char, RuntimeError> {
    match value.decode_immediate() {
        Some(crate::value::Immediate::Char(ch)) => Ok(ch),
        _ => Err(RuntimeError::InvalidObjectKind),
    }
}

fn collect_list_elements(mut value: Value) -> Result<Vec<Value>, RuntimeError> {
    let mut values = Vec::new();
    loop {
        match value.decode_immediate() {
            Some(crate::value::Immediate::EmptyList) => return Ok(values),
            Some(_) => return Err(RuntimeError::InvalidObjectKind),
            None => {}
        }

        let object = value
            .to_object_reference()
            .ok_or(RuntimeError::InvalidObjectKind)?;
        if unsafe {
            (*object
                .to_raw_address()
                .to_ptr::<crate::layout::ObjectHeader>())
            .kind
        } != crate::layout::HEADER_TAG_PAIR
        {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let pair = unsafe { &*object.to_raw_address().to_ptr::<PairObject>() };
        values.push(pair.car());
        value = pair.cdr();
    }
}

fn display_value(writer: &mut dyn Write, value: Value) -> Result<(), RuntimeError> {
    write_value(writer, value, false)
}

fn write_value(
    writer: &mut dyn Write,
    value: Value,
    quoted_strings: bool,
) -> Result<(), RuntimeError> {
    if let Some(fixnum) = value.decode_fixnum() {
        write!(writer, "{fixnum}").map_err(|error| RuntimeError::io_like(error))?;
        return Ok(());
    }

    match value.decode_immediate() {
        Some(crate::value::Immediate::Bool(true)) => {
            writer
                .write_all(b"#t")
                .map_err(|error| RuntimeError::io_like(error))?;
            return Ok(());
        }
        Some(crate::value::Immediate::Bool(false)) => {
            writer
                .write_all(b"#f")
                .map_err(|error| RuntimeError::io_like(error))?;
            return Ok(());
        }
        Some(crate::value::Immediate::Char(value)) => {
            match value {
                ' ' => writer.write_all(b"#\\space"),
                '\n' => writer.write_all(b"#\\newline"),
                value => {
                    let mut buffer = [0u8; 4];
                    let encoded = value.encode_utf8(&mut buffer);
                    writer
                        .write_all(b"#\\")
                        .and_then(|_| writer.write_all(encoded.as_bytes()))
                }
            }
            .map_err(|error| RuntimeError::io_like(error))?;
            return Ok(());
        }
        Some(crate::value::Immediate::EmptyList) => {
            writer
                .write_all(b"()")
                .map_err(|error| RuntimeError::io_like(error))?;
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
            writer
                .write_all(b"(")
                .map_err(|error| RuntimeError::io_like(error))?;
            display_pair(writer, pair)?;
            writer
                .write_all(b")")
                .map_err(|error| RuntimeError::io_like(error))?;
        }
        crate::layout::HEADER_TAG_STRING => {
            let string = unsafe { &*object.to_raw_address().to_ptr::<StringObject>() };
            let bytes = unsafe { core::slice::from_raw_parts(string.bytes_ptr(), string.length) };
            if quoted_strings {
                writer
                    .write_all(b"\"")
                    .map_err(|error| RuntimeError::io_like(error))?;
            }
            writer
                .write_all(bytes)
                .map_err(|error| RuntimeError::io_like(error))?;
            if quoted_strings {
                writer
                    .write_all(b"\"")
                    .map_err(|error| RuntimeError::io_like(error))?;
            }
        }
        crate::layout::HEADER_TAG_SYMBOL => {
            let symbol = unsafe { &*object.to_raw_address().to_ptr::<SymbolObject>() };
            let bytes = unsafe { core::slice::from_raw_parts(symbol.bytes_ptr(), symbol.length) };
            writer
                .write_all(bytes)
                .map_err(|error| RuntimeError::io_like(error))?;
        }
        crate::layout::HEADER_TAG_VECTOR => {
            let vector = unsafe { &*object.to_raw_address().to_ptr::<VectorObject>() };
            writer
                .write_all(b"#(")
                .map_err(|error| RuntimeError::io_like(error))?;
            for index in 0..vector.length {
                if index != 0 {
                    writer
                        .write_all(b" ")
                        .map_err(|error| RuntimeError::io_like(error))?;
                }
                let element = Value::from_bits(unsafe { *vector.elements_ptr().add(index) });
                write_value(writer, element, quoted_strings)?;
            }
            writer
                .write_all(b")")
                .map_err(|error| RuntimeError::io_like(error))?;
        }
        crate::layout::HEADER_TAG_BOX => {
            let boxed = unsafe { &*object.to_raw_address().to_ptr::<crate::object::BoxObject>() };
            writer
                .write_all(b"#&")
                .map_err(|error| RuntimeError::io_like(error))?;
            write_value(writer, boxed.value(), quoted_strings)?;
        }
        crate::layout::HEADER_TAG_VALUES => {
            let values = unsafe { &*object.to_raw_address().to_ptr::<ValuesObject>() };
            write!(writer, "#<values {}>", values.length)
                .map_err(|error| RuntimeError::io_like(error))?;
        }
        crate::layout::HEADER_TAG_PROMISE => {
            writer
                .write_all(b"#<promise>")
                .map_err(|error| RuntimeError::io_like(error))?;
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
                && unsafe {
                    (*object
                        .to_raw_address()
                        .to_ptr::<crate::layout::ObjectHeader>())
                    .kind
                } == crate::layout::HEADER_TAG_PAIR
            {
                let cdr_pair = unsafe { &*object.to_raw_address().to_ptr::<PairObject>() };
                writer
                    .write_all(b" ")
                    .map_err(|error| RuntimeError::io_like(error))?;
                return display_pair(writer, cdr_pair);
            }
            writer
                .write_all(b" . ")
                .map_err(|error| RuntimeError::io_like(error))?;
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
                && unsafe {
                    (*object
                        .to_raw_address()
                        .to_ptr::<crate::layout::ObjectHeader>())
                    .kind
                } == crate::layout::HEADER_TAG_PAIR
            {
                let cdr_pair = unsafe { &*object.to_raw_address().to_ptr::<PairObject>() };
                writer
                    .write_all(b" ")
                    .map_err(|error| RuntimeError::io_like(error))?;
                return write_pair(writer, cdr_pair);
            }
            writer
                .write_all(b" . ")
                .map_err(|error| RuntimeError::io_like(error))?;
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
            writer
                .write_all(b"#t")
                .map_err(|error| RuntimeError::io_like(error))?;
            return Ok(());
        }
        Some(crate::value::Immediate::Bool(false)) => {
            writer
                .write_all(b"#f")
                .map_err(|error| RuntimeError::io_like(error))?;
            return Ok(());
        }
        Some(crate::value::Immediate::Char(value)) => {
            match value {
                ' ' => writer.write_all(b"#\\space"),
                '\n' => writer.write_all(b"#\\newline"),
                value => {
                    let mut buffer = [0u8; 4];
                    let encoded = value.encode_utf8(&mut buffer);
                    writer
                        .write_all(b"#\\")
                        .and_then(|_| writer.write_all(encoded.as_bytes()))
                }
            }
            .map_err(|error| RuntimeError::io_like(error))?;
            return Ok(());
        }
        Some(crate::value::Immediate::EmptyList) => {
            writer
                .write_all(b"()")
                .map_err(|error| RuntimeError::io_like(error))?;
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
            writer
                .write_all(b"(")
                .map_err(|error| RuntimeError::io_like(error))?;
            write_pair(writer, pair)?;
            writer
                .write_all(b")")
                .map_err(|error| RuntimeError::io_like(error))?;
        }
        crate::layout::HEADER_TAG_VALUES => {
            let values = unsafe { &*object.to_raw_address().to_ptr::<ValuesObject>() };
            write!(writer, "#<values {}>", values.length)
                .map_err(|error| RuntimeError::io_like(error))?;
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
        if unsafe {
            (*object
                .to_raw_address()
                .to_ptr::<crate::layout::ObjectHeader>())
            .kind
        } != crate::layout::HEADER_TAG_PAIR
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
        if unsafe {
            (*object
                .to_raw_address()
                .to_ptr::<crate::layout::ObjectHeader>())
            .kind
        } != crate::layout::HEADER_TAG_PAIR
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
        if unsafe {
            (*object
                .to_raw_address()
                .to_ptr::<crate::layout::ObjectHeader>())
            .kind
        } != crate::layout::HEADER_TAG_PAIR
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
    if unsafe {
        (*object
            .to_raw_address()
            .to_ptr::<crate::layout::ObjectHeader>())
        .kind
    } != crate::layout::HEADER_TAG_PAIR
    {
        return Err(RuntimeError::InvalidObjectKind);
    }
    Ok(unsafe { (*object.to_raw_address().to_ptr::<PairObject>()).car() })
}

fn member_with(
    target: Value,
    mut list: Value,
    compare: impl Fn(Value, Value) -> Result<bool, RuntimeError>,
) -> Result<Value, RuntimeError> {
    loop {
        match list.decode_immediate() {
            Some(crate::value::Immediate::EmptyList) => return Ok(Value::encode_bool(false)),
            Some(_) => return Err(RuntimeError::InvalidObjectKind),
            None => {}
        }

        let object = list
            .to_object_reference()
            .ok_or(RuntimeError::InvalidObjectKind)?;
        if unsafe {
            (*object
                .to_raw_address()
                .to_ptr::<crate::layout::ObjectHeader>())
            .kind
        } != crate::layout::HEADER_TAG_PAIR
        {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let pair = unsafe { &*object.to_raw_address().to_ptr::<PairObject>() };
        if compare(target, pair.car())? {
            return Ok(list);
        }
        list = pair.cdr();
    }
}

fn assoc_with(
    target: Value,
    mut list: Value,
    compare: impl Fn(Value, Value) -> Result<bool, RuntimeError>,
) -> Result<Value, RuntimeError> {
    loop {
        match list.decode_immediate() {
            Some(crate::value::Immediate::EmptyList) => return Ok(Value::encode_bool(false)),
            Some(_) => return Err(RuntimeError::InvalidObjectKind),
            None => {}
        }

        let object = list
            .to_object_reference()
            .ok_or(RuntimeError::InvalidObjectKind)?;
        if unsafe {
            (*object
                .to_raw_address()
                .to_ptr::<crate::layout::ObjectHeader>())
            .kind
        } != crate::layout::HEADER_TAG_PAIR
        {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let pair = unsafe { &*object.to_raw_address().to_ptr::<PairObject>() };
        let candidate = pair.car();
        let candidate_object = candidate
            .to_object_reference()
            .ok_or(RuntimeError::InvalidObjectKind)?;
        if unsafe {
            (*candidate_object
                .to_raw_address()
                .to_ptr::<crate::layout::ObjectHeader>())
            .kind
        } != crate::layout::HEADER_TAG_PAIR
        {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let candidate_pair = unsafe { &*candidate_object.to_raw_address().to_ptr::<PairObject>() };
        if compare(target, candidate_pair.car())? {
            return Ok(candidate);
        }
        list = pair.cdr();
    }
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
        if unsafe {
            (*object
                .to_raw_address()
                .to_ptr::<crate::layout::ObjectHeader>())
            .kind
        } != crate::layout::HEADER_TAG_PAIR
        {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let pair = unsafe { &*object.to_raw_address().to_ptr::<PairObject>() };
        elements.push(pair.car());
        left = pair.cdr();
    }

    let thread = current_thread();
    let mut roots = RootGuard::new(thread);
    for element in &mut elements {
        roots.push(&mut element.0)?;
    }

    let mut result_bits = right.bits();
    roots.push(&mut result_bits)?;
    for index in (0..elements.len()).rev() {
        let element = elements[index];
        result_bits = Value::from_object_reference(alloc_pair_checked(
            element,
            Value::from_bits(result_bits),
        )?)
        .bits();
    }
    Ok(Value::from_bits(result_bits))
}

fn copy_list(mut value: Value) -> Result<Value, RuntimeError> {
    let mut elements = Vec::new();
    loop {
        match value.decode_immediate() {
            Some(crate::value::Immediate::EmptyList) => break,
            Some(_) => return Err(RuntimeError::InvalidObjectKind),
            None => {}
        }

        let object = value
            .to_object_reference()
            .ok_or(RuntimeError::InvalidObjectKind)?;
        if unsafe {
            (*object
                .to_raw_address()
                .to_ptr::<crate::layout::ObjectHeader>())
            .kind
        } != crate::layout::HEADER_TAG_PAIR
        {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let pair = unsafe { &*object.to_raw_address().to_ptr::<PairObject>() };
        elements.push(pair.car());
        value = pair.cdr();
    }

    let thread = current_thread();
    let mut roots = RootGuard::new(thread);
    for element in &mut elements {
        roots.push(&mut element.0)?;
    }

    let mut result_bits = Value::empty_list().bits();
    roots.push(&mut result_bits)?;
    for index in (0..elements.len()).rev() {
        let element = elements[index];
        result_bits = Value::from_object_reference(alloc_pair_checked(
            element,
            Value::from_bits(result_bits),
        )?)
        .bits();
    }
    Ok(Value::from_bits(result_bits))
}

fn reverse_list(mut value: Value) -> Result<Value, RuntimeError> {
    let mut elements = Vec::new();
    loop {
        match value.decode_immediate() {
            Some(crate::value::Immediate::EmptyList) => break,
            Some(_) => return Err(RuntimeError::InvalidObjectKind),
            None => {}
        }

        let object = value
            .to_object_reference()
            .ok_or(RuntimeError::InvalidObjectKind)?;
        if unsafe {
            (*object
                .to_raw_address()
                .to_ptr::<crate::layout::ObjectHeader>())
            .kind
        } != crate::layout::HEADER_TAG_PAIR
        {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let pair = unsafe { &*object.to_raw_address().to_ptr::<PairObject>() };
        elements.push(pair.car());
        value = pair.cdr();
    }

    let thread = current_thread();
    let mut roots = RootGuard::new(thread);
    for element in &mut elements {
        roots.push(&mut element.0)?;
    }

    let mut result_bits = Value::empty_list().bits();
    roots.push(&mut result_bits)?;
    for index in 0..elements.len() {
        let element = elements[index];
        result_bits = Value::from_object_reference(alloc_pair_checked(
            element,
            Value::from_bits(result_bits),
        )?)
        .bits();
    }
    Ok(Value::from_bits(result_bits))
}

fn equal_value(left: Value, right: Value) -> Result<bool, RuntimeError> {
    if left == right {
        return Ok(true);
    }

    match (left.decode_immediate(), right.decode_immediate()) {
        (Some(_), Some(_)) => return Ok(false),
        (Some(_), None) | (None, Some(_)) => return Ok(false),
        (None, None) => {}
    }

    let Some(left_object) = left.to_object_reference() else {
        return Ok(false);
    };
    let Some(right_object) = right.to_object_reference() else {
        return Ok(false);
    };

    let left_kind = object_kind(left)?;
    let right_kind = object_kind(right)?;
    if left_kind != right_kind {
        return Ok(false);
    }

    match left_kind {
        crate::layout::HEADER_TAG_PAIR => {
            let left_pair = unsafe { &*left_object.to_raw_address().to_ptr::<PairObject>() };
            let right_pair = unsafe { &*right_object.to_raw_address().to_ptr::<PairObject>() };
            Ok(
                equal_value(left_pair.car(), right_pair.car())?
                    && equal_value(left_pair.cdr(), right_pair.cdr())?,
            )
        }
        crate::layout::HEADER_TAG_STRING => {
            let left_string = unsafe { &*left_object.to_raw_address().to_ptr::<StringObject>() };
            let right_string = unsafe { &*right_object.to_raw_address().to_ptr::<StringObject>() };
            if left_string.length != right_string.length {
                return Ok(false);
            }
            let left_bytes =
                unsafe { core::slice::from_raw_parts(left_string.bytes_ptr(), left_string.length) };
            let right_bytes = unsafe {
                core::slice::from_raw_parts(right_string.bytes_ptr(), right_string.length)
            };
            Ok(left_bytes == right_bytes)
        }
        crate::layout::HEADER_TAG_SYMBOL => {
            let left_symbol = unsafe { &*left_object.to_raw_address().to_ptr::<SymbolObject>() };
            let right_symbol = unsafe { &*right_object.to_raw_address().to_ptr::<SymbolObject>() };
            if left_symbol.length != right_symbol.length {
                return Ok(false);
            }
            let left_bytes =
                unsafe { core::slice::from_raw_parts(left_symbol.bytes_ptr(), left_symbol.length) };
            let right_bytes = unsafe {
                core::slice::from_raw_parts(right_symbol.bytes_ptr(), right_symbol.length)
            };
            Ok(left_bytes == right_bytes)
        }
        crate::layout::HEADER_TAG_VECTOR => {
            let left_vector = unsafe { &*left_object.to_raw_address().to_ptr::<VectorObject>() };
            let right_vector = unsafe { &*right_object.to_raw_address().to_ptr::<VectorObject>() };
            if left_vector.length != right_vector.length {
                return Ok(false);
            }
            for index in 0..left_vector.length {
                let left_element =
                    Value::from_bits(unsafe { *left_vector.elements_ptr().add(index) });
                let right_element =
                    Value::from_bits(unsafe { *right_vector.elements_ptr().add(index) });
                if !equal_value(left_element, right_element)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        crate::layout::HEADER_TAG_BOX => {
            let left_box =
                unsafe { &*left_object.to_raw_address().to_ptr::<crate::object::BoxObject>() };
            let right_box =
                unsafe { &*right_object.to_raw_address().to_ptr::<crate::object::BoxObject>() };
            equal_value(left_box.value(), right_box.value())
        }
        crate::layout::HEADER_TAG_VALUES => {
            let left_values = unsafe { &*left_object.to_raw_address().to_ptr::<ValuesObject>() };
            let right_values = unsafe { &*right_object.to_raw_address().to_ptr::<ValuesObject>() };
            if left_values.length != right_values.length {
                return Ok(false);
            }
            for index in 0..left_values.length {
                let left_element =
                    Value::from_bits(unsafe { *left_values.elements_ptr().add(index) });
                let right_element =
                    Value::from_bits(unsafe { *right_values.elements_ptr().add(index) });
                if !equal_value(left_element, right_element)? {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        crate::layout::HEADER_TAG_PROMISE => Ok(false),
        crate::layout::HEADER_TAG_CLOSURE => Ok(false),
        _ => Ok(false),
    }
}

fn apply_builtin_value(id: u16, list: Value) -> Result<Value, RuntimeError> {
    let args = collect_list_elements(list)?;
    match id {
        BUILTIN_ADD => {
            let mut total = 0i64;
            for value in args {
                total = total
                    .checked_add(expect_fixnum(value)?)
                    .ok_or(RuntimeError::FixnumOutOfRange)?;
            }
            Value::encode_fixnum(total).ok_or(RuntimeError::FixnumOutOfRange)
        }
        BUILTIN_SUB => {
            let (first, rest) = args.split_first().ok_or(RuntimeError::InvalidObjectKind)?;
            let mut total = expect_fixnum(*first)?;
            if rest.is_empty() {
                total = total.checked_neg().ok_or(RuntimeError::FixnumOutOfRange)?;
            } else {
                for value in rest {
                    total = total
                        .checked_sub(expect_fixnum(*value)?)
                        .ok_or(RuntimeError::FixnumOutOfRange)?;
                }
            }
            Value::encode_fixnum(total).ok_or(RuntimeError::FixnumOutOfRange)
        }
        BUILTIN_MUL => {
            let mut total = 1i64;
            for value in args {
                total = total
                    .checked_mul(expect_fixnum(value)?)
                    .ok_or(RuntimeError::FixnumOutOfRange)?;
            }
            Value::encode_fixnum(total).ok_or(RuntimeError::FixnumOutOfRange)
        }
        BUILTIN_DIV => {
            let (first, rest) = args.split_first().ok_or(RuntimeError::InvalidObjectKind)?;
            if rest.is_empty() {
                return Err(RuntimeError::InvalidObjectKind);
            }
            let mut total = expect_fixnum(*first)?;
            for value in rest {
                let divisor = expect_fixnum(*value)?;
                total = total.checked_div(divisor).ok_or(RuntimeError::FixnumOutOfRange)?;
            }
            Value::encode_fixnum(total).ok_or(RuntimeError::FixnumOutOfRange)
        }
        BUILTIN_NOT => {
            expect_arity(&args, 1)?;
            Ok(Value::encode_bool(
                args[0].decode_immediate() == Some(crate::value::Immediate::Bool(false)),
            ))
        }
        BUILTIN_BOOLEAN_PREDICATE => {
            expect_arity(&args, 1)?;
            Ok(Value::encode_bool(matches!(
                args[0].decode_immediate(),
                Some(crate::value::Immediate::Bool(_))
            )))
        }
        BUILTIN_ZERO_PREDICATE => {
            expect_arity(&args, 1)?;
            Ok(Value::encode_bool(expect_fixnum(args[0])? == 0))
        }
        BUILTIN_CHAR_PREDICATE => {
            expect_arity(&args, 1)?;
            Ok(Value::encode_bool(matches!(
                args[0].decode_immediate(),
                Some(crate::value::Immediate::Char(_))
            )))
        }
        BUILTIN_CHAR_EQ | BUILTIN_CHAR_LT | BUILTIN_CHAR_GT | BUILTIN_CHAR_LE | BUILTIN_CHAR_GE => {
            if args.len() < 2 {
                return Err(RuntimeError::InvalidObjectKind);
            }
            let mut iter = args.into_iter().map(expect_char);
            let mut previous = iter.next().ok_or(RuntimeError::InvalidObjectKind)??;
            for current in iter {
                let current = current?;
                let ok = match id {
                    BUILTIN_CHAR_EQ => previous == current,
                    BUILTIN_CHAR_LT => previous < current,
                    BUILTIN_CHAR_GT => previous > current,
                    BUILTIN_CHAR_LE => previous <= current,
                    BUILTIN_CHAR_GE => previous >= current,
                    _ => unreachable!(),
                };
                if !ok {
                    return Ok(Value::encode_bool(false));
                }
                previous = current;
            }
            Ok(Value::encode_bool(true))
        }
        BUILTIN_CHAR_TO_INTEGER => {
            expect_arity(&args, 1)?;
            Value::encode_fixnum(expect_char(args[0])? as i64).ok_or(RuntimeError::FixnumOutOfRange)
        }
        BUILTIN_INTEGER_TO_CHAR => {
            expect_arity(&args, 1)?;
            let scalar = u32::try_from(expect_fixnum(args[0])?).map_err(|_| RuntimeError::InvalidObjectKind)?;
            let ch = char::from_u32(scalar).ok_or(RuntimeError::InvalidObjectKind)?;
            Ok(Value::encode_char(ch))
        }
        BUILTIN_SYMBOL_PREDICATE => {
            expect_arity(&args, 1)?;
            Ok(Value::encode_bool(
                object_kind(args[0]).ok() == Some(crate::layout::HEADER_TAG_SYMBOL),
            ))
        }
        BUILTIN_SYMBOL_TO_STRING => {
            expect_arity(&args, 1)?;
            Ok(Value::from_bits(mlisp_symbol_to_string(args[0].bits())))
        }
        BUILTIN_STRING_TO_SYMBOL => {
            expect_arity(&args, 1)?;
            Ok(Value::from_bits(mlisp_string_to_symbol(args[0].bits())))
        }
        BUILTIN_PROCEDURE_PREDICATE => {
            expect_arity(&args, 1)?;
            Ok(Value::encode_bool(false))
        }
        BUILTIN_EQ | BUILTIN_EQV => {
            expect_arity(&args, 2)?;
            Ok(Value::encode_bool(args[0] == args[1]))
        }
        BUILTIN_EQUAL => {
            expect_arity(&args, 2)?;
            Ok(Value::encode_bool(equal_value(args[0], args[1])?))
        }
        BUILTIN_LIST => {
            let mut rooted = args.iter().map(|value| value.bits()).collect::<Vec<_>>();
            let thread = current_thread();
            let mut roots = RootGuard::new(thread);
            for slot in &mut rooted {
                roots.push(slot)?;
            }
            let mut result_bits = Value::empty_list().bits();
            roots.push(&mut result_bits)?;
            for value in args.iter().rev().copied() {
                result_bits = Value::from_object_reference(alloc_pair_checked(
                    value,
                    Value::from_bits(result_bits),
                )?)
                .bits();
            }
            Ok(Value::from_bits(result_bits))
        }
        BUILTIN_APPEND => {
            let mut result = Value::empty_list();
            if let Some(last) = args.last().copied() {
                result = last;
            }
            for value in args[..args.len().saturating_sub(1)].iter().rev().copied() {
                result = append_two_lists(value, result)?;
            }
            Ok(result)
        }
        BUILTIN_MEMQ => {
            expect_arity(&args, 2)?;
            member_with(args[0], args[1], |left, right| Ok(left == right))
        }
        BUILTIN_MEMV => {
            expect_arity(&args, 2)?;
            member_with(args[0], args[1], |left, right| Ok(left == right))
        }
        BUILTIN_MEMBER => {
            expect_arity(&args, 2)?;
            member_with(args[0], args[1], equal_value)
        }
        BUILTIN_ASSQ => {
            expect_arity(&args, 2)?;
            assoc_with(args[0], args[1], |left, right| Ok(left == right))
        }
        BUILTIN_ASSV => {
            expect_arity(&args, 2)?;
            assoc_with(args[0], args[1], |left, right| Ok(left == right))
        }
        BUILTIN_ASSOC => {
            expect_arity(&args, 2)?;
            assoc_with(args[0], args[1], equal_value)
        }
        BUILTIN_LIST_COPY => {
            expect_arity(&args, 1)?;
            copy_list(args[0])
        }
        BUILTIN_REVERSE => {
            expect_arity(&args, 1)?;
            reverse_list(args[0])
        }
        BUILTIN_CONS => {
            expect_arity(&args, 2)?;
            Ok(Value::from_object_reference(alloc_pair_checked(args[0], args[1])?))
        }
        BUILTIN_CAR => {
            expect_arity(&args, 1)?;
            let object = args[0]
                .to_object_reference()
                .ok_or(RuntimeError::InvalidObjectKind)?;
            if object_kind(args[0])? != crate::layout::HEADER_TAG_PAIR {
                return Err(RuntimeError::InvalidObjectKind);
            }
            Ok(unsafe { (*object.to_raw_address().to_ptr::<PairObject>()).car() })
        }
        BUILTIN_CDR => {
            expect_arity(&args, 1)?;
            let object = args[0]
                .to_object_reference()
                .ok_or(RuntimeError::InvalidObjectKind)?;
            if object_kind(args[0])? != crate::layout::HEADER_TAG_PAIR {
                return Err(RuntimeError::InvalidObjectKind);
            }
            Ok(unsafe { (*object.to_raw_address().to_ptr::<PairObject>()).cdr() })
        }
        BUILTIN_SET_CAR => {
            expect_arity(&args, 2)?;
            let result = mlisp_pair_set_car(args[0].bits(), args[1].bits());
            Ok(Value::from_bits(result))
        }
        BUILTIN_SET_CDR => {
            expect_arity(&args, 2)?;
            let result = mlisp_pair_set_cdr(args[0].bits(), args[1].bits());
            Ok(Value::from_bits(result))
        }
        BUILTIN_PAIR_PREDICATE => {
            expect_arity(&args, 1)?;
            Ok(Value::encode_bool(
                object_kind(args[0]).ok() == Some(crate::layout::HEADER_TAG_PAIR),
            ))
        }
        BUILTIN_LIST_PREDICATE => {
            expect_arity(&args, 1)?;
            Ok(Value::encode_bool(is_proper_list(args[0])))
        }
        BUILTIN_LENGTH => {
            expect_arity(&args, 1)?;
            let len = proper_list_length(args[0])?;
            Value::encode_fixnum(len as i64).ok_or(RuntimeError::FixnumOutOfRange)
        }
        BUILTIN_LIST_TAIL => {
            expect_arity(&args, 2)?;
            list_tail_value(args[0], usize::try_from(expect_fixnum(args[1])?).map_err(|_| RuntimeError::IndexOutOfBounds)?)
        }
        BUILTIN_LIST_REF => {
            expect_arity(&args, 2)?;
            list_ref_value(args[0], usize::try_from(expect_fixnum(args[1])?).map_err(|_| RuntimeError::IndexOutOfBounds)?)
        }
        BUILTIN_NULL_PREDICATE => {
            expect_arity(&args, 1)?;
            Ok(Value::encode_bool(
                args[0].decode_immediate() == Some(crate::value::Immediate::EmptyList),
            ))
        }
        BUILTIN_STRING_PREDICATE => {
            expect_arity(&args, 1)?;
            Ok(Value::encode_bool(
                object_kind(args[0]).ok() == Some(crate::layout::HEADER_TAG_STRING),
            ))
        }
        BUILTIN_STRING_LENGTH => {
            expect_arity(&args, 1)?;
            Ok(Value::from_bits(mlisp_string_length(args[0].bits())))
        }
        BUILTIN_STRING_REF => {
            expect_arity(&args, 2)?;
            Ok(Value::from_bits(mlisp_string_ref(
                args[0].bits(),
                usize::try_from(expect_fixnum(args[1])?).map_err(|_| RuntimeError::IndexOutOfBounds)?,
            )))
        }
        BUILTIN_DISPLAY => {
            expect_arity(&args, 1)?;
            let mut handle = io::stdout().lock();
            display_value(&mut handle, args[0])?;
            Ok(Value::unspecified())
        }
        BUILTIN_WRITE => {
            expect_arity(&args, 1)?;
            let mut handle = io::stdout().lock();
            write_value(&mut handle, args[0], true)?;
            Ok(Value::unspecified())
        }
        BUILTIN_NEWLINE => {
            expect_arity(&args, 0)?;
            io::stdout()
                .lock()
                .write_all(b"\n")
                .map_err(RuntimeError::io_like)?;
            Ok(Value::unspecified())
        }
        BUILTIN_GC_STRESS => {
            expect_arity(&args, 1)?;
            gc_stress_checked(expect_fixnum(args[0])? as usize)?;
            Ok(Value::unspecified())
        }
        BUILTIN_VECTOR => {
            let elements = args;
            Ok(Value::from_object_reference(alloc_vector_checked(&elements)?))
        }
        BUILTIN_VECTOR_PREDICATE => {
            expect_arity(&args, 1)?;
            Ok(Value::encode_bool(
                object_kind(args[0]).ok() == Some(crate::layout::HEADER_TAG_VECTOR),
            ))
        }
        BUILTIN_VECTOR_LENGTH => {
            expect_arity(&args, 1)?;
            Ok(Value::from_bits(mlisp_vector_length(args[0].bits())))
        }
        BUILTIN_VECTOR_REF => {
            expect_arity(&args, 2)?;
            Ok(Value::from_bits(mlisp_vector_ref(
                args[0].bits(),
                usize::try_from(expect_fixnum(args[1])?).map_err(|_| RuntimeError::IndexOutOfBounds)?,
            )))
        }
        BUILTIN_VECTOR_SET => {
            expect_arity(&args, 3)?;
            Ok(Value::from_bits(mlisp_vector_set(
                args[0].bits(),
                usize::try_from(expect_fixnum(args[1])?).map_err(|_| RuntimeError::IndexOutOfBounds)?,
                args[2].bits(),
            )))
        }
        BUILTIN_RAISE => {
            expect_arity(&args, 1)?;
            raise_checked(args[0])?;
            Ok(Value::unspecified())
        }
        BUILTIN_ERROR => {
            if args.is_empty() {
                return Err(RuntimeError::InvalidArgument);
            }
            let error = make_error_value(&args)?;
            raise_checked(error)?;
            Ok(Value::unspecified())
        }
        _ => Err(RuntimeError::InvalidObjectKind),
    }
}

fn values_length(value: Value) -> Result<usize, RuntimeError> {
    let object = value
        .to_object_reference()
        .ok_or(RuntimeError::InvalidObjectKind)?;
    if object_kind(value)? != crate::layout::HEADER_TAG_VALUES {
        return Err(RuntimeError::InvalidObjectKind);
    }
    Ok(unsafe { (*object.to_raw_address().to_ptr::<ValuesObject>()).length })
}

fn values_ref(value: Value, index: usize) -> Result<Value, RuntimeError> {
    let object = value
        .to_object_reference()
        .ok_or(RuntimeError::InvalidObjectKind)?;
    if object_kind(value)? != crate::layout::HEADER_TAG_VALUES {
        return Err(RuntimeError::InvalidObjectKind);
    }
    let values = unsafe { &*object.to_raw_address().to_ptr::<ValuesObject>() };
    if index >= values.length {
        return Err(RuntimeError::IndexOutOfBounds);
    }
    Ok(Value::from_bits(unsafe { *values.elements_ptr().add(index) }))
}

fn values_tail_list(value: Value, start: usize) -> Result<Value, RuntimeError> {
    let object = value
        .to_object_reference()
        .ok_or(RuntimeError::InvalidObjectKind)?;
    if object_kind(value)? != crate::layout::HEADER_TAG_VALUES {
        return Err(RuntimeError::InvalidObjectKind);
    }
    let values = unsafe { &*object.to_raw_address().to_ptr::<ValuesObject>() };
    if start > values.length {
        return Err(RuntimeError::IndexOutOfBounds);
    }

    let thread = current_thread();
    let mut rooted_values = value.bits();
    let mut result_bits = Value::empty_list().bits();
    let mut roots = RootGuard::new(thread);
    roots.push(&mut rooted_values)?;
    roots.push(&mut result_bits)?;

    for index in (start..values.length).rev() {
        let element = Value::from_bits(unsafe { *values.elements_ptr().add(index) });
        result_bits = Value::from_object_reference(alloc_pair_checked(
            element,
            Value::from_bits(result_bits),
        )?)
        .bits();
    }

    Ok(Value::from_bits(result_bits))
}

fn make_error_value(args: &[Value]) -> Result<Value, RuntimeError> {
    let symbol = intern_symbol(b"error")?;
    let thread = current_thread();
    let mut rooted = args.iter().map(|value| value.bits()).collect::<Vec<_>>();
    let mut symbol_bits = symbol.bits();
    let mut result_bits = Value::empty_list().bits();
    let mut roots = RootGuard::new(thread);
    for slot in &mut rooted {
        roots.push(slot)?;
    }
    roots.push(&mut symbol_bits)?;
    roots.push(&mut result_bits)?;

    for value in args.iter().rev().copied() {
        result_bits = Value::from_object_reference(alloc_pair_checked(
            value,
            Value::from_bits(result_bits),
        )?)
        .bits();
    }
    result_bits = Value::from_object_reference(alloc_pair_checked(
        Value::from_bits(symbol_bits),
        Value::from_bits(result_bits),
    )?)
    .bits();
    Ok(Value::from_bits(result_bits))
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
pub extern "C" fn rt_exception_pending() -> bool {
    ffi_bool(exception_pending_checked)
}

#[unsafe(no_mangle)]
pub extern "C" fn rt_raise(value: usize) -> usize {
    ffi_word(|| {
        raise_checked(Value::from_bits(value))?;
        Ok(Value::unspecified().bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn rt_take_pending_exception() -> usize {
    ffi_word(|| Ok(take_pending_exception_checked()?.bits()))
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
        let Some(src) =
            mmtk::util::ObjectReference::from_raw_address(mmtk::util::Address::from_mut_ptr(src))
        else {
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
        handle
            .flush()
            .map_err(|error| RuntimeError::io_like(error))?;
        Ok(Value::unspecified().bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_write(value: usize) -> usize {
    ffi_word(|| {
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        write_scheme_value(&mut handle, Value::from_bits(value))?;
        handle
            .flush()
            .map_err(|error| RuntimeError::io_like(error))?;
        Ok(Value::unspecified().bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_newline() -> usize {
    ffi_word(|| {
        let stdout = io::stdout();
        let mut handle = stdout.lock();
        handle
            .write_all(b"\n")
            .map_err(|error| RuntimeError::io_like(error))?;
        handle
            .flush()
            .map_err(|error| RuntimeError::io_like(error))?;
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
    ffi_word(|| {
        Ok(Value::from_object_reference(alloc_box_checked(Value::from_bits(value))?).bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_alloc_promise(value: usize) -> usize {
    ffi_word(|| {
        Ok(Value::from_object_reference(alloc_promise_checked(Value::from_bits(value))?).bits())
    })
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
pub extern "C" fn mlisp_alloc_promise_gc(value: usize) -> *mut PromiseObject {
    ffi_ptr(|| {
        Ok(alloc_promise_checked(Value::from_bits(value))?
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
pub extern "C" fn mlisp_promise_forced(value: usize) -> bool {
    ffi_bool(|| {
        let value = Value::from_bits(value);
        if object_kind(value)? != crate::layout::HEADER_TAG_PROMISE {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let object = value.to_object_reference().ok_or(RuntimeError::InvalidObjectKind)?;
        Ok(unsafe { (*object.to_raw_address().to_ptr::<PromiseObject>()).is_forced() })
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_promise_value(value: usize) -> usize {
    ffi_word(|| {
        let value = Value::from_bits(value);
        if object_kind(value)? != crate::layout::HEADER_TAG_PROMISE {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let object = value.to_object_reference().ok_or(RuntimeError::InvalidObjectKind)?;
        Ok(unsafe { (*object.to_raw_address().to_ptr::<PromiseObject>()).value() }.bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_promise_resolve(value: usize, resolved: usize) -> usize {
    ffi_word(|| {
        let value = Value::from_bits(value);
        if object_kind(value)? != crate::layout::HEADER_TAG_PROMISE {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let object = value.to_object_reference().ok_or(RuntimeError::InvalidObjectKind)?;
        let promise = object.to_raw_address().to_mut_ptr::<PromiseObject>();
        object_write_post_checked(
            object,
            unsafe { core::ptr::addr_of_mut!((*promise).value) },
            Value::from_bits(resolved),
        )?;
        unsafe { (*promise).forced = 1 };
        Ok(resolved)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_promise_forced_gc(promise: *mut PromiseObject) -> bool {
    ffi_bool(|| {
        if promise.is_null() {
            return Err(RuntimeError::InvalidObjectKind);
        }
        Ok(unsafe { (*promise).is_forced() })
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_promise_value_gc(promise: *mut PromiseObject) -> usize {
    ffi_word(|| {
        if promise.is_null() {
            return Err(RuntimeError::InvalidObjectKind);
        }
        Ok(unsafe { (*promise).value() }.bits())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_promise_resolve_gc(
    promise: *mut PromiseObject,
    resolved: usize,
) -> usize {
    ffi_word(|| {
        if promise.is_null() {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let object = mmtk::util::ObjectReference::from_raw_address(
            mmtk::util::Address::from_mut_ptr(promise.cast::<core::ffi::c_void>()),
        )
        .ok_or(RuntimeError::InvalidObjectKind)?;
        object_write_post_checked(
            object,
            unsafe { core::ptr::addr_of_mut!((*promise).value) },
            Value::from_bits(resolved),
        )?;
        unsafe { (*promise).forced = 1 };
        Ok(resolved)
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_alloc_closure(
    code_ptr: usize,
    env_values: *const usize,
    env_len: usize,
) -> usize {
    ffi_word(|| {
        if env_len != 0 && env_values.is_null() {
            return Err(RuntimeError::NullSlot);
        }
        let slice = if env_len == 0 {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(env_values, env_len) }
        };
        let env = slice
            .iter()
            .copied()
            .map(Value::from_bits)
            .collect::<Vec<_>>();
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
        let env = slice
            .iter()
            .copied()
            .map(Value::from_bits)
            .collect::<Vec<_>>();
        Ok(alloc_closure_checked(code_ptr, &env)?
            .to_raw_address()
            .to_mut_ptr())
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
pub unsafe extern "C" fn mlisp_closure_env_ref_gc(
    closure: *mut ClosureObject,
    index: usize,
) -> usize {
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
        Ok(intern_symbol(slice)?.bits())
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
        let value = intern_symbol(slice)?;
        Ok(value
            .to_object_reference()
            .ok_or(RuntimeError::InvalidObjectKind)?
            .to_raw_address()
            .to_mut_ptr())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_is_string(value: usize) -> bool {
    ffi_bool(|| {
        Ok(object_kind(Value::from_bits(value)).ok() == Some(crate::layout::HEADER_TAG_STRING))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_is_symbol(value: usize) -> bool {
    ffi_bool(|| {
        Ok(object_kind(Value::from_bits(value)).ok() == Some(crate::layout::HEADER_TAG_SYMBOL))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_symbol_to_string(value: usize) -> usize {
    ffi_word(|| {
        let value = Value::from_bits(value);
        let object = value
            .to_object_reference()
            .ok_or(RuntimeError::InvalidObjectKind)?;
        if object_kind(value)? != crate::layout::HEADER_TAG_SYMBOL {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let mut rooted = value.bits();
        let thread = current_thread();
        let mut roots = RootGuard::new(thread);
        roots.push(&mut rooted)?;
        let symbol = unsafe { &*object.to_raw_address().to_ptr::<SymbolObject>() };
        let bytes = unsafe { core::slice::from_raw_parts(symbol.bytes_ptr(), symbol.length) };
        Ok(Value::from_object_reference(alloc_string_checked(bytes)?).bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_string_to_symbol(value: usize) -> usize {
    ffi_word(|| {
        let value = Value::from_bits(value);
        let object = value
            .to_object_reference()
            .ok_or(RuntimeError::InvalidObjectKind)?;
        if object_kind(value)? != crate::layout::HEADER_TAG_STRING {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let mut rooted = value.bits();
        let thread = current_thread();
        let mut roots = RootGuard::new(thread);
        roots.push(&mut rooted)?;
        let string = unsafe { &*object.to_raw_address().to_ptr::<StringObject>() };
        let bytes = unsafe { core::slice::from_raw_parts(string.bytes_ptr(), string.length) };
        Ok(intern_symbol(bytes)?.bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_equal(left: usize, right: usize) -> bool {
    ffi_bool(|| equal_value(Value::from_bits(left), Value::from_bits(right)))
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_apply_builtin(id: usize, args: usize) -> usize {
    ffi_word(|| Ok(apply_builtin_value(id as u16, Value::from_bits(args))?.bits()))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_alloc_values(values: *const usize, len: usize) -> usize {
    ffi_word(|| {
        if len != 0 && values.is_null() {
            return Err(RuntimeError::NullSlot);
        }
        let slice = if len == 0 {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(values, len) }
        };
        let elements = slice
            .iter()
            .copied()
            .map(Value::from_bits)
            .collect::<Vec<_>>();
        Ok(Value::from_object_reference(alloc_values_checked(&elements)?).bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_is_values(value: usize) -> bool {
    ffi_bool(|| {
        Ok(object_kind(Value::from_bits(value)).ok() == Some(crate::layout::HEADER_TAG_VALUES))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_values_length(value: usize) -> usize {
    ffi_word(|| {
        let length = i64::try_from(values_length(Value::from_bits(value))?)
            .map_err(|_| RuntimeError::FixnumOutOfRange)?;
        Value::encode_fixnum(length)
            .ok_or(RuntimeError::FixnumOutOfRange)
            .map(Value::bits)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_values_ref(value: usize, index: usize) -> usize {
    ffi_word(|| Ok(values_ref(Value::from_bits(value), index)?.bits()))
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_values_tail_list(value: usize, start: usize) -> usize {
    ffi_word(|| Ok(values_tail_list(Value::from_bits(value), start)?.bits()))
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
        let elements = slice
            .iter()
            .copied()
            .map(Value::from_bits)
            .collect::<Vec<_>>();
        Ok(Value::from_object_reference(alloc_vector_checked(&elements)?).bits())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_alloc_vector_gc(
    values: *const usize,
    len: usize,
) -> *mut VectorObject {
    ffi_ptr(|| {
        if len != 0 && values.is_null() {
            return Err(RuntimeError::NullSlot);
        }
        let slice = if len == 0 {
            &[]
        } else {
            unsafe { core::slice::from_raw_parts(values, len) }
        };
        let elements = slice
            .iter()
            .copied()
            .map(Value::from_bits)
            .collect::<Vec<_>>();
        Ok(alloc_vector_checked(&elements)?
            .to_raw_address()
            .to_mut_ptr())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_is_vector(value: usize) -> bool {
    ffi_bool(|| {
        Ok(object_kind(Value::from_bits(value)).ok() == Some(crate::layout::HEADER_TAG_VECTOR))
    })
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
        object_write_post_checked(
            object,
            unsafe { vector.elements_mut_ptr().add(index) },
            Value::from_bits(element),
        )?;
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
        Ok(
            alloc_pair_checked(Value::from_bits(car), Value::from_bits(cdr))?
                .to_raw_address()
                .to_mut_ptr(),
        )
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
pub extern "C" fn mlisp_pair_set_car(value: usize, element: usize) -> usize {
    ffi_word(|| {
        let pair = Value::from_bits(value)
            .to_object_reference()
            .ok_or(RuntimeError::InvalidObjectKind)?;
        if object_kind(Value::from_bits(value))? != crate::layout::HEADER_TAG_PAIR {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let pair_ptr = pair.to_raw_address().to_mut_ptr::<PairObject>();
        object_write_post_checked(
            pair,
            unsafe { core::ptr::addr_of_mut!((*pair_ptr).car) },
            Value::from_bits(element),
        )?;
        Ok(Value::unspecified().bits())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_pair_set_car_gc(pair: *mut PairObject, element: usize) -> usize {
    ffi_word(|| {
        if pair.is_null() {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let object = mmtk::util::ObjectReference::from_raw_address(
            mmtk::util::Address::from_mut_ptr(pair),
        )
        .ok_or(RuntimeError::InvalidObjectKind)?;
        object_write_post_checked(
            object,
            unsafe { core::ptr::addr_of_mut!((*pair).car) },
            Value::from_bits(element),
        )?;
        Ok(Value::unspecified().bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_pair_set_cdr(value: usize, element: usize) -> usize {
    ffi_word(|| {
        let pair = Value::from_bits(value)
            .to_object_reference()
            .ok_or(RuntimeError::InvalidObjectKind)?;
        if object_kind(Value::from_bits(value))? != crate::layout::HEADER_TAG_PAIR {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let pair_ptr = pair.to_raw_address().to_mut_ptr::<PairObject>();
        object_write_post_checked(
            pair,
            unsafe { core::ptr::addr_of_mut!((*pair_ptr).cdr) },
            Value::from_bits(element),
        )?;
        Ok(Value::unspecified().bits())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn mlisp_pair_set_cdr_gc(pair: *mut PairObject, element: usize) -> usize {
    ffi_word(|| {
        if pair.is_null() {
            return Err(RuntimeError::InvalidObjectKind);
        }
        let object = mmtk::util::ObjectReference::from_raw_address(
            mmtk::util::Address::from_mut_ptr(pair),
        )
        .ok_or(RuntimeError::InvalidObjectKind)?;
        object_write_post_checked(
            object,
            unsafe { core::ptr::addr_of_mut!((*pair).cdr) },
            Value::from_bits(element),
        )?;
        Ok(Value::unspecified().bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_is_pair(value: usize) -> bool {
    ffi_bool(|| {
        Ok(object_kind(Value::from_bits(value)).ok() == Some(crate::layout::HEADER_TAG_PAIR))
    })
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

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_memq(value: usize, list: usize) -> usize {
    ffi_word(|| {
        Ok(member_with(Value::from_bits(value), Value::from_bits(list), |left, right| {
            Ok(left == right)
        })?
        .bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_memv(value: usize, list: usize) -> usize {
    ffi_word(|| {
        Ok(member_with(Value::from_bits(value), Value::from_bits(list), |left, right| {
            Ok(left == right)
        })?
        .bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_member(value: usize, list: usize) -> usize {
    ffi_word(|| {
        Ok(member_with(
            Value::from_bits(value),
            Value::from_bits(list),
            equal_value,
        )?
        .bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_assq(value: usize, list: usize) -> usize {
    ffi_word(|| {
        Ok(assoc_with(Value::from_bits(value), Value::from_bits(list), |left, right| {
            Ok(left == right)
        })?
        .bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_assv(value: usize, list: usize) -> usize {
    ffi_word(|| {
        Ok(assoc_with(Value::from_bits(value), Value::from_bits(list), |left, right| {
            Ok(left == right)
        })?
        .bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_assoc(value: usize, list: usize) -> usize {
    ffi_word(|| {
        Ok(assoc_with(
            Value::from_bits(value),
            Value::from_bits(list),
            equal_value,
        )?
        .bits())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_list_copy(value: usize) -> usize {
    ffi_word(|| Ok(copy_list(Value::from_bits(value))?.bits()))
}

#[unsafe(no_mangle)]
pub extern "C" fn mlisp_reverse(value: usize) -> usize {
    ffi_word(|| Ok(reverse_list(Value::from_bits(value))?.bits()))
}

#[cfg(test)]
mod tests {
    use super::{
        mlisp_alloc_pair, mlisp_alloc_pair_gc, mlisp_alloc_string, mlisp_alloc_string_gc,
        mlisp_alloc_values, mlisp_alloc_vector, mlisp_alloc_vector_gc, mlisp_append, mlisp_is_list,
        mlisp_is_pair, mlisp_is_string, mlisp_is_symbol, mlisp_is_values, mlisp_is_vector,
        mlisp_list_copy, mlisp_list_length, mlisp_list_ref, mlisp_list_tail, mlisp_make_fixnum,
        mlisp_pair_car, mlisp_pair_car_gc, mlisp_pair_cdr, mlisp_pair_cdr_gc, mlisp_pair_set_car,
        mlisp_pair_set_cdr, mlisp_reverse, mlisp_string_length, mlisp_string_length_gc,
        mlisp_string_ref, mlisp_string_ref_gc, mlisp_string_to_symbol, mlisp_symbol_to_string,
        mlisp_values_length, mlisp_values_ref, mlisp_values_tail_list, mlisp_vector_length,
        mlisp_vector_length_gc, mlisp_vector_ref, mlisp_vector_ref_gc, mlisp_vector_set,
        mlisp_vector_set_gc, rt_bind_thread, rt_exception_pending, rt_gc_poll, rt_mmtk_init,
        rt_raise, rt_run_mutator_stress, rt_take_pending_exception, rt_unbind_thread,
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
        assert_eq!(
            Value::from_bits(mlisp_list_length(list)).decode_fixnum(),
            Some(2)
        );
        assert_eq!(
            Value::from_bits(mlisp_list_ref(list, 1)).decode_fixnum(),
            Some(2)
        );
        assert!(mlisp_is_list(mlisp_list_tail(list, 1)));
        assert_eq!(mlisp_list_length(dotted), Value::unspecified().bits());
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
        assert_eq!(
            Value::from_bits(mlisp_string_length(value)).decode_fixnum(),
            Some(5)
        );
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
        assert_eq!(
            Value::from_bits(unsafe { mlisp_string_length_gc(string) }).decode_fixnum(),
            Some(2)
        );
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
    fn converts_between_symbols_and_strings() {
        assert!(rt_mmtk_init(8 * 1024 * 1024, 1));
        let thread = rt_bind_thread();
        let symbol = unsafe { super::mlisp_alloc_symbol(b"hello".as_ptr(), 5) };
        let string = mlisp_symbol_to_string(symbol);
        assert!(mlisp_is_string(string));
        let roundtrip = mlisp_string_to_symbol(string);
        assert!(mlisp_is_symbol(roundtrip));
        unsafe { rt_unbind_thread(thread) };
    }

    #[test]
    fn allocates_and_reads_values_packets_through_runtime() {
        assert!(rt_mmtk_init(8 * 1024 * 1024, 1));
        let thread = rt_bind_thread();
        let elements = [mlisp_make_fixnum(1), mlisp_make_fixnum(2), mlisp_make_fixnum(3)];
        let values = unsafe { mlisp_alloc_values(elements.as_ptr(), elements.len()) };
        assert!(mlisp_is_values(values));
        assert_eq!(
            Value::from_bits(mlisp_values_length(values)).decode_fixnum(),
            Some(3)
        );
        assert_eq!(
            Value::from_bits(mlisp_values_ref(values, 1)).decode_fixnum(),
            Some(2)
        );
        let tail = mlisp_values_tail_list(values, 1);
        assert!(mlisp_is_list(tail));
        assert_eq!(
            Value::from_bits(mlisp_list_ref(tail, 0)).decode_fixnum(),
            Some(2)
        );
        rt_gc_poll();
        assert_eq!(
            Value::from_bits(mlisp_values_ref(values, 2)).decode_fixnum(),
            Some(3)
        );
        unsafe { rt_unbind_thread(thread) };
    }

    #[test]
    fn raises_and_takes_pending_exceptions_through_runtime() {
        assert!(rt_mmtk_init(8 * 1024 * 1024, 1));
        let thread = rt_bind_thread();
        let value = mlisp_make_fixnum(42);
        assert_eq!(rt_raise(value), Value::unspecified().bits());
        assert!(rt_exception_pending());
        assert_eq!(rt_take_pending_exception(), value);
        assert!(!rt_exception_pending());
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
        assert_eq!(
            Value::from_bits(mlisp_list_length(appended)).decode_fixnum(),
            Some(3)
        );
        assert_eq!(
            Value::from_bits(mlisp_list_ref(appended, 2)).decode_fixnum(),
            Some(3)
        );
        unsafe { rt_unbind_thread(thread) };
    }

    #[test]
    fn finds_members_and_assoc_pairs_through_runtime() {
        assert!(rt_mmtk_init(8 * 1024 * 1024, 1));
        let thread = rt_bind_thread();
        let empty = Value::empty_list().bits();
        let symbol_a = unsafe { super::mlisp_alloc_symbol(b"a".as_ptr(), 1) };
        let symbol_b = unsafe { super::mlisp_alloc_symbol(b"b".as_ptr(), 1) };
        let symbol_c = unsafe { super::mlisp_alloc_symbol(b"c".as_ptr(), 1) };
        let c_tail = mlisp_alloc_pair(symbol_c, empty);
        let b_tail = mlisp_alloc_pair(symbol_b, c_tail);
        let list = mlisp_alloc_pair(symbol_a, b_tail);
        let found = super::mlisp_memq(symbol_b, list);
        assert!(mlisp_is_list(found));

        let pair_b = mlisp_alloc_pair(
            symbol_b,
            mlisp_make_fixnum(2),
        );
        let alist = mlisp_alloc_pair(pair_b, empty);
        let assoc = super::mlisp_assq(symbol_b, alist);
        assert!(mlisp_is_pair(assoc));
        unsafe { rt_unbind_thread(thread) };
    }

    #[test]
    fn mutates_and_copies_lists_through_runtime() {
        assert!(rt_mmtk_init(8 * 1024 * 1024, 1));
        let thread = rt_bind_thread();
        let empty = Value::empty_list().bits();
        let tail = mlisp_alloc_pair(mlisp_make_fixnum(2), empty);
        let pair = mlisp_alloc_pair(mlisp_make_fixnum(1), tail);
        let mut rooted_pair = pair;
        super::push_root_checked(thread, &mut rooted_pair).unwrap();
        assert_eq!(
            mlisp_pair_set_car(rooted_pair, mlisp_make_fixnum(9)),
            Value::unspecified().bits()
        );
        assert_eq!(mlisp_pair_set_cdr(rooted_pair, empty), Value::unspecified().bits());
        assert!(mlisp_is_pair(rooted_pair));
        assert_eq!(
            Value::from_bits(unsafe { mlisp_pair_car(rooted_pair) }).decode_fixnum(),
            Some(9)
        );
        assert_eq!(unsafe { mlisp_pair_cdr(rooted_pair) }, empty);

        let copied = mlisp_list_copy(mlisp_append(rooted_pair, tail));
        let reversed = mlisp_reverse(copied);
        assert_eq!(
            Value::from_bits(mlisp_list_ref(reversed, 0)).decode_fixnum(),
            Some(2)
        );
        assert_eq!(
            Value::from_bits(mlisp_list_ref(reversed, 1)).decode_fixnum(),
            Some(9)
        );
        super::pop_root_checked(thread).unwrap();
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
        assert_eq!(
            Value::from_bits(mlisp_vector_length(vector)).decode_fixnum(),
            Some(2)
        );
        assert_eq!(
            Value::from_bits(mlisp_vector_ref(vector, 0)).decode_fixnum(),
            Some(7)
        );
        assert_eq!(mlisp_vector_ref(vector, 1), string);

        let replacement = mlisp_alloc_pair(mlisp_make_fixnum(1), mlisp_make_fixnum(2));
        assert_eq!(
            mlisp_vector_set(vector, 1, replacement),
            Value::unspecified().bits()
        );
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
        assert_eq!(
            unsafe { mlisp_pair_car_gc(core::ptr::null_mut()) },
            Value::unspecified().bits()
        );
        unsafe { rt_unbind_thread(core::ptr::null_mut()) };
    }
}
