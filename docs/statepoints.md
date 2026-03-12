# Statepoint Plan

`mlisp` is targeting LLVM statepoints rather than `gc.root`.

The key conventions are:

- GC-managed references live in `addrspace(1)`.
- Functions that participate in GC use a GC strategy string, currently
  `"statepoint-example"` as a placeholder.
- Safepoints are explicit `llvm.experimental.gc.statepoint` calls.
- Live GC references are attached through the `"gc-live"` operand bundle.
- Any reference used after a safepoint must be reloaded through
  `llvm.experimental.gc.relocate`.

Minimal example:

```llvm
define ptr addrspace(1) @test(ptr addrspace(1) %obj) gc "statepoint-example" {
entry:
  %safepoint = call token (i64, i32, ptr, i32, i32, ...)
    @llvm.experimental.gc.statepoint.p0f_isVoidf(
      i64 0, i32 0, ptr elementtype(void ()) @foo, i32 0, i32 0, i32 0, i32 0
    ) ["gc-live"(ptr addrspace(1) %obj)]
  %obj.relocated = call ptr addrspace(1)
    @llvm.experimental.gc.relocate.p1(token %safepoint, i32 0, i32 0)
  ret ptr addrspace(1) %obj.relocated
}
```

The exact fixture is checked into
[examples/llvm/statepoint_example.ll](/home/du/code/mlisp/examples/llvm/statepoint_example.ll).

For the safepoint-placement pipeline, the repo also includes a pre-statepoint
input fixture in
[examples/llvm/place_safepoints_input.ll](/home/du/code/mlisp/examples/llvm/place_safepoints_input.ll)
and a helper script in
[scripts/verify-statepoint-pipeline.sh](/home/du/code/mlisp/scripts/verify-statepoint-pipeline.sh)
that runs:

```bash
opt -passes="function(place-safepoints),rewrite-statepoints-for-gc"
```

The pre-statepoint fixture includes the polling symbols LLVM expects:

```llvm
define void @gc.safepoint_poll() {
entry:
  call void @gc_safepoint_poll()
  ret void
}

declare void @gc_safepoint_poll()
```

Without those, `place-safepoints` does not rewrite the function correctly and
may crash depending on toolchain behavior.

## Consequences For `mlisp`

This changes the compiler/runtime contract in an important way:

- GC references must remain typed as `ptr addrspace(1)` in LLVM IR.
- A raw tagged `i64` alone is not enough for relocatable roots across safepoints.
- Frontend values and runtime values can still be "Scheme values", but LLVM IR
  needs a split representation:
  - immediates as integers
  - GC references as address-space-1 pointers

In other words, the current single-word `Value` runtime representation remains
useful at the runtime ABI boundary, but statepoint-aware codegen should not keep
all live Scheme values flattened to `i64` if they might contain movable heap
references.

## What This Means For Closures

Closures should eventually lower to:

- code pointer
- environment pointer in `addrspace(1)`

Captured locals that survive allocation points must become fields in a heap
environment object. After each safepoint, any live environment or object
pointer must be taken from `gc.relocate`.

## Inkwell Constraint

`inkwell` exposes:

- function GC strings via `FunctionValue::set_gc`
- custom address spaces
- operand bundles on normal calls

But it does not model LLVM token types cleanly, which makes direct
statepoint construction incomplete through `inkwell` alone. For full
statepoint emission, `mlisp` will need a thin `llvm-sys` layer or textual IR
assembly for the specific statepoint intrinsics.

## LLVM 21 Note

On the currently installed LLVM 21.1.8 toolchain in this workspace:

- handwritten statepoint IR verifies correctly with `verify<safepoint-ir>`
- `rewrite-statepoints-for-gc` is available as a standalone pass
- the pipeline form `function(place-safepoints),rewrite-statepoints-for-gc`
  parses correctly
- the pipeline succeeds on the checked-in pre-statepoint fixture when the
  `gc.safepoint_poll` and `gc_safepoint_poll` symbols are present

So the project now tracks two things separately:

- the intended pipeline command
- the LLVM 21 behavior actually observed locally

Both are now covered by tests:

- handwritten statepoint verification
- pre-statepoint rewriting through `place-safepoints` plus
  `rewrite-statepoints-for-gc`

## Next Compiler Steps

1. Split codegen values into immediates and GC pointers.
2. Introduce heap object values in `addrspace(1)`.
3. Add a low-level statepoint builder around `llvm-sys`.
4. Lower allocations and unknown calls through explicit safepoints.
5. Replace the placeholder GC strategy name with the real MMTk-compatible one.
