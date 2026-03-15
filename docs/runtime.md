# Runtime Plan

This project targets R7RS small semantics with an LLVM backend and an eventual
MMTk-managed heap. R7RS does not mandate a specific collector, but it does
require object lifetime and control-flow behavior that rule out purely
stack-bound representations for core Scheme values.

## Goals

- Keep immediate scalars cheap.
- Heap-allocate language objects whose lifetime is graph-shaped.
- Preserve compatibility with a moving collector.
- Leave room for proper tail calls and future continuation support.
- Make LLVM lowering expose explicit roots instead of relying on Rust lifetimes.

## Tagged Value Layout

`mlisp` uses a single machine-word `Value`.

- Fixnums: low bit `1`
- Heap references: aligned pointers, low three bits `000`
- Immediates: reserved non-zero tag patterns

Current immediate set:

- `#f`
- `#t`
- `'()`
- unspecified result

This is defined in [src/runtime/layout.rs](./src/runtime/layout.rs) and
[src/runtime/value.rs](./src/runtime/value.rs).

## Heap Objects

The initial heap object catalog is:

- pairs
- boxes for mutable cells
- closures
- vectors
- strings
- symbols

All heap objects begin with a compact header carrying:

- object kind
- flags
- byte size

That gives the runtime enough structure for tracing, relocation, and future
object-specific scanning behavior.

## MMTk Boundary

The current MMTk integration point is intentionally a boundary, not a live
dependency yet. The project now defines:

- a `GarbageCollector` trait for runtime allocation entrypoints
- an `MmtkRuntime` plan type describing roots and allocation assumptions

See [src/runtime/mmtk.rs](./src/runtime/mmtk.rs).

The expected runtime shape for real MMTk integration is:

1. LLVM-generated code calls a small runtime ABI such as `mlisp_alloc_pair`.
2. The runtime records roots via stack maps or a shadow stack.
3. MMTk owns tracing, movement, and reclamation.
4. Generated code treats heap references as movable and reloadable values.

The project now includes a concrete host ABI scaffold in
[src/runtime/abi.rs](./src/runtime/abi.rs) and LLVM-side
declarations in [src/backend/runtime.rs](./src/backend/runtime.rs).
The current allocator is intentionally a stub backed by Rust heap allocation so
the ABI can stabilize before the collector is swapped to MMTk.

## LLVM Implications

To stay compatible with MMTk, future codegen should avoid baking raw object
addresses into long-lived SSA values across safepoints without root tracking.

Planned compiler/runtime contract:

- Immediate values remain plain machine words.
- Heap values are also machine words, but treated as GC references.
- Every call that may allocate is a safepoint boundary.
- Locals live in a root-visible location when they must survive allocation.
- Closure conversion materializes captured locals into heap environments.

For the statepoint-specific lowering direction, see
[docs/statepoints.md](./docs/statepoints.md).

## Next Runtime Steps

1. Add a runtime ABI module for allocation and primitive operations.
2. Introduce a closure environment object layout and closure conversion pass.
3. Represent mutable top-level bindings and `set!` via boxes/cells.
4. Decide on root strategy: LLVM stack maps vs explicit shadow stack.
5. Replace the stub allocator with a real MMTk-backed implementation.
