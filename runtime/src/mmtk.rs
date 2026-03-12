use crate::error::RuntimeError;
use crate::layout::{ALIGNMENT, ObjectHeader};
use crate::object::{BoxObject, ClosureObject, HeapKind, PairObject, StringObject, SymbolObject, VectorObject};
use crate::value::Value;
use mmtk::memory_manager;
use mmtk::plan::{AllocationSemantics, Mutator};
use mmtk::util::alloc::AllocationError;
use mmtk::util::copy::{CopySemantics, GCWorkerCopyContext};
use mmtk::util::opaque_pointer::{OpaquePointer, VMMutatorThread, VMThread, VMWorkerThread};
use mmtk::util::options::{GCTriggerSelector, PlanSelector};
use mmtk::util::{Address, ObjectReference};
use mmtk::vm::slot::{MemorySlice, Slot};
use mmtk::vm::{
    ActivePlan, Collection, GCThreadContext, ObjectModel, ReferenceGlue, RootsWorkFactory,
    Scanning, SlotVisitor, VMBinding,
};
use mmtk::{MMTKBuilder, MMTK};
use std::cell::Cell;
use std::cell::UnsafeCell;
use std::fmt;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};

thread_local! {
    static CURRENT_THREAD: Cell<usize> = const { Cell::new(0) };
}

static RUNTIME: OnceLock<RuntimeState> = OnceLock::new();
static INIT_MMTK: OnceLock<&'static MMTK<MlispVM>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct ValueSlot(usize);

unsafe impl Send for ValueSlot {}

impl ValueSlot {
    pub fn from_ptr(slot: *mut usize) -> Self {
        Self(slot as usize)
    }
}

impl Slot for ValueSlot {
    fn load(&self) -> Option<ObjectReference> {
        let bits = unsafe { ptr::read(self.0 as *const usize) };
        Value::from_bits(bits).to_object_reference()
    }

    fn store(&self, object: ObjectReference) {
        unsafe { ptr::write(self.0 as *mut usize, object.to_raw_address().as_usize()) }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ValueMemorySlice {
    start: usize,
    words: usize,
}

impl ValueMemorySlice {
    pub fn new(start: *mut usize, words: usize) -> Self {
        Self {
            start: start as usize,
            words,
        }
    }
}

unsafe impl Send for ValueMemorySlice {}

impl MemorySlice for ValueMemorySlice {
    type SlotType = ValueSlot;
    type SlotIterator = ValueMemorySliceIter;

    fn iter_slots(&self) -> Self::SlotIterator {
        ValueMemorySliceIter {
            cursor: self.start,
            remaining: self.words,
        }
    }

    fn object(&self) -> Option<ObjectReference> {
        None
    }

    fn start(&self) -> Address {
        unsafe { Address::from_usize(self.start) }
    }

    fn bytes(&self) -> usize {
        self.words * core::mem::size_of::<usize>()
    }

    fn copy(src: &Self, dst: &Self) {
        assert_eq!(src.words, dst.words);
        unsafe {
            ptr::copy_nonoverlapping(src.start as *const usize, dst.start as *mut usize, src.words)
        };
    }
}

pub struct ValueMemorySliceIter {
    cursor: usize,
    remaining: usize,
}

impl Iterator for ValueMemorySliceIter {
    type Item = ValueSlot;

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }

        let current = self.cursor;
        self.cursor += core::mem::size_of::<usize>();
        self.remaining -= 1;
        Some(ValueSlot(current))
    }
}

pub struct MlispVM;

impl Default for MlispVM {
    fn default() -> Self {
        Self
    }
}

impl VMBinding for MlispVM {
    type VMObjectModel = Self;
    type VMScanning = Self;
    type VMCollection = Self;
    type VMActivePlan = Self;
    type VMReferenceGlue = Self;
    type VMSlot = ValueSlot;
    type VMMemorySlice = ValueMemorySlice;

    const MIN_ALIGNMENT: usize = ALIGNMENT;
    const MAX_ALIGNMENT: usize = ALIGNMENT;
    const USE_ALLOCATION_OFFSET: bool = false;
    const ALLOC_END_ALIGNMENT: usize = ALIGNMENT;
}

struct RuntimeState {
    mmtk: &'static MMTK<MlispVM>,
    threads: Mutex<Vec<usize>>,
    global_roots: Mutex<Vec<usize>>,
    gc: GcCoordinator,
}

struct GcCoordinator {
    state: Mutex<GcState>,
    wake: Condvar,
}

struct GcState {
    requested: bool,
    epoch: u64,
}

struct ThreadContext {
    tls: VMMutatorThread,
    mutator: *mut Mutator<MlispVM>,
    roots: Mutex<Vec<usize>>,
    blocked_epoch: AtomicU64,
    active: AtomicBool,
}

unsafe impl Send for ThreadContext {}
unsafe impl Sync for ThreadContext {}

struct SharedRoot(UnsafeCell<usize>);

unsafe impl Sync for SharedRoot {}

impl SharedRoot {
    fn new() -> Self {
        Self(UnsafeCell::new(0))
    }

    fn as_ptr(&self) -> *mut usize {
        self.0.get()
    }
}

struct RootStackGuard {
    thread: *mut core::ffi::c_void,
    count: usize,
}

impl RootStackGuard {
    fn new(thread: *mut core::ffi::c_void) -> Self {
        Self { thread, count: 0 }
    }

    fn push(&mut self, slot: *mut usize) -> Result<(), RuntimeError> {
        push_root_checked(self.thread, slot)?;
        self.count += 1;
        Ok(())
    }
}

impl Drop for RootStackGuard {
    fn drop(&mut self) {
        while self.count > 0 {
            let _ = pop_root_checked(self.thread);
            self.count -= 1;
        }
    }
}

fn runtime() -> &'static RuntimeState {
    RUNTIME.get().ok_or(RuntimeError::NotInitialized).unwrap()
}

fn runtime_mmtk() -> &'static MMTK<MlispVM> {
    if let Some(runtime) = RUNTIME.get() {
        runtime.mmtk
    } else {
        *INIT_MMTK
            .get()
            .ok_or(RuntimeError::NotInitialized)
            .unwrap()
    }
}

fn runtime_checked() -> Result<&'static RuntimeState, RuntimeError> {
    RUNTIME.get().ok_or(RuntimeError::NotInitialized)
}

fn lock_unpoison<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_unpoison<'a, T>(condvar: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    condvar
        .wait(guard)
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn current_thread() -> *mut core::ffi::c_void {
    CURRENT_THREAD.with(|slot| slot.get() as *mut core::ffi::c_void)
}

fn ctx_from_vm_thread(tls: VMThread) -> &'static ThreadContext {
    ctx_from_vm_thread_checked(tls).unwrap()
}

fn current_thread_context_checked() -> Result<&'static ThreadContext, RuntimeError> {
    let current = current_thread();
    if current.is_null() {
        return Err(RuntimeError::ThreadNotBound);
    }
    Ok(unsafe { &*(current as *mut ThreadContext) })
}

fn ctx_from_vm_thread_checked(tls: VMThread) -> Result<&'static ThreadContext, RuntimeError> {
    let address = tls.0.to_address().as_usize();
    if address < core::mem::align_of::<ThreadContext>()
        || address % core::mem::align_of::<ThreadContext>() != 0
    {
        return Err(RuntimeError::InvalidThread);
    }
    let ptr = tls.0.to_address().to_mut_ptr::<ThreadContext>();
    if ptr.is_null() {
        return Err(RuntimeError::InvalidThread);
    }
    Ok(unsafe { &*ptr })
}

fn ensure_initialized(
    heap_size_bytes: usize,
    worker_count: usize,
) -> &'static RuntimeState {
    RUNTIME.get_or_init(|| {
        let mut builder = MMTKBuilder::new();
        builder.options.plan.set(PlanSelector::StickyImmix);
        builder
            .options
            .gc_trigger
            .set(GCTriggerSelector::FixedHeapSize(heap_size_bytes));
        builder.options.threads.set(worker_count.max(1));
        let mmtk = Box::leak(memory_manager::mmtk_init::<MlispVM>(&builder));
        let _ = INIT_MMTK.set(mmtk);
        memory_manager::initialize_collection(mmtk, VMThread::UNINITIALIZED);
        RuntimeState {
            mmtk,
            threads: Mutex::new(Vec::new()),
            global_roots: Mutex::new(Vec::new()),
            gc: GcCoordinator {
                state: Mutex::new(GcState {
                    requested: false,
                    epoch: 0,
                }),
                wake: Condvar::new(),
            },
        }
    })
}

fn stop_for_gc(thread: &'static ThreadContext) {
    let runtime = runtime();
    let mut state = lock_unpoison(&runtime.gc.state);
    if !state.requested {
        return;
    }

    let epoch = state.epoch;
    thread.blocked_epoch.store(epoch, Ordering::SeqCst);
    runtime.gc.wake.notify_all();
    while state.requested && thread.active.load(Ordering::SeqCst) {
        state = wait_unpoison(&runtime.gc.wake, state);
    }
    thread.blocked_epoch.store(0, Ordering::SeqCst);
}

fn bind_current_thread() -> *mut ThreadContext {
    let runtime = ensure_initialized(64 * 1024 * 1024, 1);
    let context = Box::new(ThreadContext {
        tls: VMMutatorThread(VMThread(OpaquePointer::UNINITIALIZED)),
        mutator: ptr::null_mut(),
        roots: Mutex::new(Vec::new()),
        blocked_epoch: AtomicU64::new(0),
        active: AtomicBool::new(true),
    });
    let raw = Box::into_raw(context);
    let tls = VMMutatorThread(VMThread(OpaquePointer::from_address(Address::from_mut_ptr(raw))));
    let mutator = Box::into_raw(memory_manager::bind_mutator(runtime.mmtk, tls));

    unsafe {
        (*raw).tls = tls;
        (*raw).mutator = mutator;
    }

    lock_unpoison(&runtime.threads).push(raw as usize);
    CURRENT_THREAD.with(|slot| slot.set(raw as usize));
    raw
}

unsafe fn unbind_thread(thread: *mut ThreadContext) {
    if thread.is_null() {
        return;
    }

    let runtime = runtime();
    CURRENT_THREAD.with(|slot| {
        if slot.get() == thread as usize {
            slot.set(0);
        }
    });

    unsafe {
        if !(*thread).mutator.is_null() {
            let mutator = &mut *(*thread).mutator;
            memory_manager::destroy_mutator(mutator);
            drop(Box::from_raw((*thread).mutator));
        }
        (*thread).active.store(false, Ordering::SeqCst);
    }

    runtime
        .threads
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .retain(|entry| *entry != thread as usize);
    runtime.gc.wake.notify_all();
    unsafe { drop(Box::from_raw(thread)) };
}

fn push_root(thread: *mut ThreadContext, slot: *mut usize) {
    push_root_checked(thread.cast(), slot).unwrap();
}

fn pop_root(thread: *mut ThreadContext) {
    pop_root_checked(thread.cast()).unwrap();
}

pub fn register_global_root(slot: *mut usize) {
    register_global_root_checked(slot).unwrap();
}

pub fn gc_poll_current() {
    gc_poll_current_checked().unwrap();
}

pub fn gc_poll_current_checked() -> Result<(), RuntimeError> {
    let runtime = runtime_checked()?;
    let thread = current_thread_context_checked()?;
    stop_for_gc(thread);
    memory_manager::gc_poll(runtime.mmtk, thread.tls);
    Ok(())
}

pub fn alloc_raw(size: usize, align: usize, kind: u16) -> ObjectReference {
    alloc_raw_checked(size, align, kind).unwrap()
}

pub fn alloc_raw_checked(
    size: usize,
    align: usize,
    kind: u16,
) -> Result<ObjectReference, RuntimeError> {
    let thread = current_thread_context_checked()?;
    let mutator = unsafe { &mut *thread.mutator };
    let addr = memory_manager::alloc_slow(mutator, size, align, 0, AllocationSemantics::Default);
    if addr.is_zero() {
        return Err(RuntimeError::AllocationFailed);
    }
    let object = ObjectReference::from_raw_address(addr).ok_or(RuntimeError::AllocationFailed)?;
    memory_manager::post_alloc(mutator, object, size, AllocationSemantics::Default);
    unsafe {
        ptr::write(
            addr.to_mut_ptr::<ObjectHeader>(),
            ObjectHeader::new(kind, size as u32),
        );
    }
    Ok(object)
}

pub fn alloc_pair(car: Value, cdr: Value) -> ObjectReference {
    alloc_pair_checked(car, cdr).unwrap()
}

pub fn alloc_pair_checked(car: Value, cdr: Value) -> Result<ObjectReference, RuntimeError> {
    let thread = current_thread();
    let mut rooted_car = car.bits();
    let mut rooted_cdr = cdr.bits();
    let mut roots = RootStackGuard::new(thread);
    roots.push(&mut rooted_car)?;
    roots.push(&mut rooted_cdr)?;
    let object = alloc_raw_checked(
        core::mem::size_of::<PairObject>(),
        core::mem::align_of::<PairObject>(),
        HeapKind::Pair.as_tag(),
    )?;
    unsafe {
        let pair = object.to_raw_address().to_mut_ptr::<PairObject>();
        (*pair).car = rooted_car;
        (*pair).cdr = rooted_cdr;
    }
    Ok(object)
}

pub fn alloc_box(value: Value) -> ObjectReference {
    alloc_box_checked(value).unwrap()
}

pub fn alloc_box_checked(value: Value) -> Result<ObjectReference, RuntimeError> {
    let thread = current_thread();
    let mut rooted_value = value.bits();
    let mut roots = RootStackGuard::new(thread);
    roots.push(&mut rooted_value)?;
    let object = alloc_raw_checked(
        core::mem::size_of::<BoxObject>(),
        core::mem::align_of::<BoxObject>(),
        HeapKind::Box.as_tag(),
    )?;
    unsafe {
        let boxed = object.to_raw_address().to_mut_ptr::<BoxObject>();
        (*boxed).value = rooted_value;
    }
    Ok(object)
}

pub fn alloc_closure(code_ptr: usize, env: &[Value]) -> ObjectReference {
    alloc_closure_checked(code_ptr, env).unwrap()
}

pub fn alloc_closure_checked(
    code_ptr: usize,
    env: &[Value],
) -> Result<ObjectReference, RuntimeError> {
    let thread = current_thread();
    let mut rooted_env = env.to_vec();
    let mut roots = RootStackGuard::new(thread);
    for value in &mut rooted_env {
        roots.push(&mut value.0)?;
    }
    let size = core::mem::size_of::<ClosureObject>() + (env.len() * core::mem::size_of::<usize>());
    let object = alloc_raw_checked(
        size,
        core::mem::align_of::<ClosureObject>(),
        HeapKind::Closure.as_tag(),
    )?;
    unsafe {
        let closure = object.to_raw_address().to_mut_ptr::<ClosureObject>();
        ptr::write(closure, ClosureObject::new(code_ptr, env.len(), size));
        ptr::copy_nonoverlapping(
            rooted_env.as_ptr().cast::<usize>(),
            (*closure).env_mut_ptr(),
            rooted_env.len(),
        );
    }
    Ok(object)
}

pub fn alloc_string_checked(bytes: &[u8]) -> Result<ObjectReference, RuntimeError> {
    let size = core::mem::size_of::<StringObject>() + bytes.len();
    let object = alloc_raw_checked(
        size,
        core::mem::align_of::<StringObject>(),
        HeapKind::String.as_tag(),
    )?;
    unsafe {
        let string = object.to_raw_address().to_mut_ptr::<StringObject>();
        ptr::write(string, StringObject::new(bytes.len(), size));
        ptr::copy_nonoverlapping(bytes.as_ptr(), (*string).bytes_mut_ptr(), bytes.len());
    }
    Ok(object)
}

pub fn alloc_symbol_checked(bytes: &[u8]) -> Result<ObjectReference, RuntimeError> {
    let size = core::mem::size_of::<SymbolObject>() + bytes.len();
    let object = alloc_raw_checked(
        size,
        core::mem::align_of::<SymbolObject>(),
        HeapKind::Symbol.as_tag(),
    )?;
    unsafe {
        let symbol = object.to_raw_address().to_mut_ptr::<SymbolObject>();
        ptr::write(symbol, SymbolObject::new(bytes.len(), size));
        ptr::copy_nonoverlapping(bytes.as_ptr(), (*symbol).bytes_mut_ptr(), bytes.len());
    }
    Ok(object)
}

pub fn alloc_vector_checked(elements: &[Value]) -> Result<ObjectReference, RuntimeError> {
    let thread = current_thread();
    let mut rooted_elements = elements.to_vec();
    let mut roots = RootStackGuard::new(thread);
    for value in &mut rooted_elements {
        roots.push(&mut value.0)?;
    }
    let size = core::mem::size_of::<VectorObject>() + (elements.len() * core::mem::size_of::<usize>());
    let object = alloc_raw_checked(
        size,
        core::mem::align_of::<VectorObject>(),
        HeapKind::Vector.as_tag(),
    )?;
    unsafe {
        let vector = object.to_raw_address().to_mut_ptr::<VectorObject>();
        ptr::write(vector, VectorObject::new(elements.len(), size));
        ptr::copy_nonoverlapping(
            rooted_elements.as_ptr().cast::<usize>(),
            (*vector).elements_mut_ptr(),
            rooted_elements.len(),
        );
    }
    Ok(object)
}

pub fn object_write_post(src: ObjectReference, slot: *mut usize, target: Value) {
    object_write_post_checked(src, slot, target).unwrap();
}

pub fn object_write_post_checked(
    src: ObjectReference,
    slot: *mut usize,
    target: Value,
) -> Result<(), RuntimeError> {
    if slot.is_null() {
        return Err(RuntimeError::NullSlot);
    }
    unsafe { ptr::write(slot, target.bits()) };
    if target.is_heap_ref() {
        let thread = current_thread_context_checked()?;
        memory_manager::object_reference_write_post(
            unsafe { &mut *thread.mutator },
            src,
            ValueSlot::from_ptr(slot),
            target.to_object_reference(),
        );
    }
    Ok(())
}

pub fn run_mutator_stress(thread_count: usize, iterations: usize) {
    run_mutator_stress_checked(thread_count, iterations).unwrap();
}

pub fn run_mutator_stress_checked(
    thread_count: usize,
    iterations: usize,
) -> Result<(), RuntimeError> {
    use std::thread;

    ensure_initialized(16 * 1024 * 1024, thread_count.max(1));

    let shared: Arc<Vec<SharedRoot>> = Arc::new(
        (0..thread_count.max(1))
            .map(|_| SharedRoot::new())
            .collect::<Vec<_>>(),
    );
    for slot in shared.iter() {
        register_global_root_checked(slot.as_ptr())?;
    }

    let mut handles = Vec::with_capacity(thread_count.max(1));
    for index in 0..thread_count.max(1) {
        let shared = Arc::clone(&shared);
        handles.push(thread::spawn(move || {
            let worker = || -> Result<(), RuntimeError> {
            let thread = bind_current_thread();
            let mut local_root = 0usize;
            push_root_checked(thread.cast(), &mut local_root)?;
            for iteration in 0..iterations {
                gc_poll_current_checked()?;
                let object = alloc_pair(
                    Value::encode_fixnum(index as i64).ok_or(RuntimeError::FixnumOutOfRange)?,
                    Value::encode_fixnum(iteration as i64).ok_or(RuntimeError::FixnumOutOfRange)?,
                );
                local_root = object.to_raw_address().as_usize();
                let slot_ptr = shared[index].as_ptr();
                unsafe {
                    let pair = object.to_raw_address().to_mut_ptr::<PairObject>();
                    object_write_post_checked(
                        object,
                        ptr::addr_of_mut!((*pair).cdr),
                        Value::from_bits(*slot_ptr),
                    )?;
                }
                unsafe { ptr::write(slot_ptr, local_root) };
            }
            pop_root_checked(thread.cast())?;
            unsafe { unbind_thread(thread) };
            Ok(())
            };
            worker()
        }));
    }

    for handle in handles {
        match handle.join() {
            Ok(result) => result?,
            Err(_) => return Err(RuntimeError::WorkerThreadPanicked),
        }
    }
    Ok(())
}

pub fn gc_stress_checked(iterations: usize) -> Result<(), RuntimeError> {
    let thread = current_thread_context_checked()?;
    let mut local_root = 0usize;
    push_root_checked(thread as *const ThreadContext as *mut core::ffi::c_void, &mut local_root)?;

    for iteration in 0..iterations {
        gc_poll_current_checked()?;
        let fixnum = Value::encode_fixnum(iteration as i64).ok_or(RuntimeError::FixnumOutOfRange)?;
        let string = alloc_string_checked(b"gc")?;
        let vector = alloc_vector_checked(&[
            fixnum,
            Value::from_object_reference(string),
            Value::empty_list(),
        ])?;
        let pair = alloc_pair_checked(
            Value::from_object_reference(vector),
            Value::from_object_reference(string),
        )?;
        let pair_value = Value::from_object_reference(pair);
        if local_root != pair_value.bits() {
            local_root = pair_value.bits();
        }
    }

    pop_root_checked(thread as *const ThreadContext as *mut core::ffi::c_void)?;
    Ok(())
}

pub fn push_root_checked(
    thread: *mut core::ffi::c_void,
    slot: *mut usize,
) -> Result<(), RuntimeError> {
    if thread.is_null() {
        return Err(RuntimeError::InvalidThread);
    }
    if slot.is_null() {
        return Err(RuntimeError::NullSlot);
    }
    unsafe { lock_unpoison(&(*(thread as *mut ThreadContext)).roots).push(slot as usize) };
    Ok(())
}

pub fn pop_root_checked(thread: *mut core::ffi::c_void) -> Result<(), RuntimeError> {
    if thread.is_null() {
        return Err(RuntimeError::InvalidThread);
    }
    let popped = unsafe { lock_unpoison(&(*(thread as *mut ThreadContext)).roots).pop() };
    if popped.is_none() {
        return Err(RuntimeError::ShadowStackUnderflow);
    }
    Ok(())
}

pub fn register_global_root_checked(slot: *mut usize) -> Result<(), RuntimeError> {
    if slot.is_null() {
        return Err(RuntimeError::NullSlot);
    }
    lock_unpoison(&runtime_checked()?.global_roots).push(slot as usize);
    Ok(())
}

impl ActivePlan<MlispVM> for MlispVM {
    fn is_mutator(tls: VMThread) -> bool {
        let Ok(thread) = ctx_from_vm_thread_checked(tls) else {
            return false;
        };
        !thread.mutator.is_null() && thread.active.load(Ordering::SeqCst)
    }

    fn mutator(tls: VMMutatorThread) -> &'static mut Mutator<MlispVM> {
        let thread = ctx_from_vm_thread(tls.0);
        unsafe { &mut *thread.mutator }
    }

    fn mutators<'a>() -> Box<dyn Iterator<Item = &'a mut Mutator<MlispVM>> + 'a> {
        let pointers: Vec<*mut Mutator<MlispVM>> = runtime()
            .threads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter_map(|thread| unsafe {
                let thread = *thread as *mut ThreadContext;
                if (*thread).active.load(Ordering::SeqCst) {
                    Some((*thread).mutator)
                } else {
                    None
                }
            })
            .collect();
        Box::new(
            pointers
                .into_iter()
                .map(|ptr| unsafe { &mut *ptr }),
        )
    }

    fn number_of_mutators() -> usize {
        runtime()
            .threads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|thread| unsafe {
                let thread = **thread as *mut ThreadContext;
                (*thread).active.load(Ordering::SeqCst)
            })
            .count()
    }
}

impl Collection<MlispVM> for MlispVM {
    fn stop_all_mutators<F>(_tls: VMWorkerThread, mutator_visitor: F)
    where
        F: FnMut(&'static mut Mutator<MlispVM>),
    {
        let runtime = runtime();
        let mut state = lock_unpoison(&runtime.gc.state);
        state.epoch += 1;
        state.requested = true;
        let epoch = state.epoch;
        runtime.gc.wake.notify_all();
        loop {
            let all_blocked = lock_unpoison(&runtime.threads).iter().all(|thread| unsafe {
                let thread = &*((*thread) as *mut ThreadContext);
                !thread.active.load(Ordering::SeqCst)
                    || thread.blocked_epoch.load(Ordering::SeqCst) == epoch
            });
            if all_blocked {
                break;
            }
            state = wait_unpoison(&runtime.gc.wake, state);
        }
        drop(state);

        let mut visitor = mutator_visitor;
        for mutator in Self::mutators() {
            visitor(mutator);
        }
    }

    fn resume_mutators(_tls: VMWorkerThread) {
        let runtime = runtime();
        let mut state = lock_unpoison(&runtime.gc.state);
        state.requested = false;
        runtime.gc.wake.notify_all();
    }

    fn block_for_gc(tls: VMMutatorThread) {
        stop_for_gc(ctx_from_vm_thread(tls.0));
    }

    fn spawn_gc_thread(_tls: VMThread, ctx: GCThreadContext<MlispVM>) {
        match ctx {
            GCThreadContext::Worker(worker) => {
                std::thread::spawn(move || {
                    let worker_tls = VMWorkerThread(VMThread(OpaquePointer::from_address(
                        Address::from_mut_ptr(Box::into_raw(Box::new(0usize))),
                    )));
                    worker.run(worker_tls, runtime_mmtk());
                });
            }
        }
    }

    fn out_of_memory(_tls: VMThread, err_kind: AllocationError) {
        panic!("MMTk allocation failed: {err_kind:?}");
    }
}

impl Scanning<MlispVM> for MlispVM {
    fn scan_object<SV: SlotVisitor<ValueSlot>>(
        _tls: VMWorkerThread,
        object: ObjectReference,
        slot_visitor: &mut SV,
    ) {
        unsafe {
            let header = &*object.to_raw_address().to_ptr::<ObjectHeader>();
            match header.kind {
                tag if tag == HeapKind::Pair.as_tag() => {
                    let pair = object.to_raw_address().to_mut_ptr::<PairObject>();
                    slot_visitor.visit_slot(ValueSlot::from_ptr(ptr::addr_of_mut!((*pair).car)));
                    slot_visitor.visit_slot(ValueSlot::from_ptr(ptr::addr_of_mut!((*pair).cdr)));
                }
                tag if tag == HeapKind::Box.as_tag() => {
                    let boxed = object.to_raw_address().to_mut_ptr::<BoxObject>();
                    slot_visitor.visit_slot(ValueSlot::from_ptr(ptr::addr_of_mut!((*boxed).value)));
                }
                tag if tag == HeapKind::Closure.as_tag() => {
                    let closure = object.to_raw_address().to_mut_ptr::<ClosureObject>();
                    for index in 0..(*closure).env_len {
                        slot_visitor.visit_slot(ValueSlot::from_ptr((*closure).env_mut_ptr().add(index)));
                    }
                }
                tag if tag == HeapKind::Vector.as_tag() => {
                    let vector = object.to_raw_address().to_mut_ptr::<VectorObject>();
                    for index in 0..(*vector).length {
                        slot_visitor.visit_slot(ValueSlot::from_ptr((*vector).elements_mut_ptr().add(index)));
                    }
                }
                tag if tag == HeapKind::String.as_tag() || tag == HeapKind::Symbol.as_tag() => {}
                _ => {}
            }
        }
    }

    fn notify_initial_thread_scan_complete(_partial_scan: bool, _tls: VMWorkerThread) {}

    fn scan_roots_in_mutator_thread(
        _tls: VMWorkerThread,
        mutator: &'static mut Mutator<MlispVM>,
        mut factory: impl RootsWorkFactory<ValueSlot>,
    ) {
        let thread = ctx_from_vm_thread(mutator.mutator_tls.0);
        let roots = lock_unpoison(&thread.roots).clone();
        factory.create_process_roots_work(
            roots
                .into_iter()
                .map(|slot| ValueSlot::from_ptr(slot as *mut usize))
                .collect(),
        );
    }

    fn scan_vm_specific_roots(_tls: VMWorkerThread, mut factory: impl RootsWorkFactory<ValueSlot>) {
        let roots = lock_unpoison(&runtime().global_roots).clone();
        factory.create_process_roots_work(
            roots
                .into_iter()
                .map(|slot| ValueSlot::from_ptr(slot as *mut usize))
                .collect(),
        );
    }

    fn supports_return_barrier() -> bool {
        false
    }

    fn prepare_for_roots_re_scanning() {}
}

impl ObjectModel<MlispVM> for MlispVM {
    const GLOBAL_LOG_BIT_SPEC: mmtk::vm::VMGlobalLogBitSpec =
        mmtk::vm::VMGlobalLogBitSpec::in_header(64);
    const LOCAL_FORWARDING_POINTER_SPEC: mmtk::vm::VMLocalForwardingPointerSpec =
        mmtk::vm::VMLocalForwardingPointerSpec::in_header(0);
    const LOCAL_FORWARDING_BITS_SPEC: mmtk::vm::VMLocalForwardingBitsSpec =
        mmtk::vm::VMLocalForwardingBitsSpec::in_header(0);
    const LOCAL_MARK_BIT_SPEC: mmtk::vm::VMLocalMarkBitSpec =
        mmtk::vm::VMLocalMarkBitSpec::in_header(65);
    const LOCAL_LOS_MARK_NURSERY_SPEC: mmtk::vm::VMLocalLOSMarkNurserySpec =
        mmtk::vm::VMLocalLOSMarkNurserySpec::in_header(66);
    const OBJECT_REF_OFFSET_LOWER_BOUND: isize = 0;
    const UNIFIED_OBJECT_REFERENCE_ADDRESS: bool = true;

    fn copy(
        from: ObjectReference,
        semantics: CopySemantics,
        copy_context: &mut GCWorkerCopyContext<MlispVM>,
    ) -> ObjectReference {
        let bytes = Self::get_current_size(from);
        let to = copy_context.alloc_copy(from, bytes, ALIGNMENT, 0, semantics);
        unsafe {
            ptr::copy_nonoverlapping(
                from.to_raw_address().to_ptr::<u8>(),
                to.to_mut_ptr::<u8>(),
                bytes,
            );
        }
        let new_object = ObjectReference::from_raw_address(to).unwrap();
        copy_context.post_copy(new_object, bytes, semantics);
        new_object
    }

    fn copy_to(from: ObjectReference, to: ObjectReference, region: Address) -> Address {
        let bytes = Self::get_current_size(from);
        unsafe {
            ptr::copy_nonoverlapping(
                from.to_raw_address().to_ptr::<u8>(),
                to.to_raw_address().to_mut_ptr::<u8>(),
                bytes,
            );
        }
        region + bytes
    }

    fn get_reference_when_copied_to(_from: ObjectReference, to: Address) -> ObjectReference {
        ObjectReference::from_raw_address(to).unwrap()
    }

    fn get_current_size(object: ObjectReference) -> usize {
        unsafe { (*object.to_raw_address().to_ptr::<ObjectHeader>()).bytes as usize }
    }

    fn get_size_when_copied(object: ObjectReference) -> usize {
        Self::get_current_size(object)
    }

    fn get_align_when_copied(_object: ObjectReference) -> usize {
        ALIGNMENT
    }

    fn get_align_offset_when_copied(_object: ObjectReference) -> usize {
        0
    }

    fn get_type_descriptor(_reference: ObjectReference) -> &'static [i8] {
        static TYPE_DESC: [i8; 6] = [109, 108, 105, 115, 112, 0];
        &TYPE_DESC
    }

    fn ref_to_object_start(object: ObjectReference) -> Address {
        object.to_raw_address()
    }

    fn ref_to_header(object: ObjectReference) -> Address {
        object.to_raw_address()
    }

    fn dump_object(object: ObjectReference) {
        eprintln!("mlisp object @ {:?}", object);
    }
}

impl ReferenceGlue<MlispVM> for MlispVM {
    type FinalizableType = ObjectReference;

    fn clear_referent(_new_reference: ObjectReference) {}

    fn get_referent(_object: ObjectReference) -> Option<ObjectReference> {
        None
    }

    fn set_referent(_reff: ObjectReference, _referent: ObjectReference) {}

    fn enqueue_references(_references: &[ObjectReference], _tls: VMWorkerThread) {}
}

impl fmt::Debug for ThreadContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ThreadContext")
            .field("tls", &self.tls)
            .field("mutator", &self.mutator)
            .finish()
    }
}

pub fn initialize_runtime(heap_size_bytes: usize, worker_count: usize) {
    let _ = ensure_initialized(heap_size_bytes, worker_count);
}

pub fn bind_thread_handle() -> *mut core::ffi::c_void {
    bind_current_thread().cast()
}

pub unsafe fn unbind_thread_handle(thread: *mut core::ffi::c_void) {
    unsafe { unbind_thread(thread.cast()) };
}

pub fn push_root_handle(thread: *mut core::ffi::c_void, slot: *mut usize) {
    push_root(thread.cast(), slot);
}

pub fn pop_root_handle(thread: *mut core::ffi::c_void) {
    pop_root(thread.cast());
}
