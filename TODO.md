# TODO

## R7RS Small Gaps

### Core Semantics
- [x] Proper tail recursion as a real guarantee.
  Scheme calls now go through the uniform wrapper/trampoline ABI (`rt_trampoline_apply` for ordinary calls, `rt_tail_invoke` for tail positions), and deep self-recursive plus mutual-recursive e2e cases run without stack growth.
- [x] `apply` for known-signature user procedures and closures.
- [x] First-class builtin procedures so primitives can participate in `apply`, `map`, and higher-order calls.
- [x] Variadic procedures.
- [x] Dotted parameter lists.
- [x] `equal?`.
- [x] Multiple values: `values`, `call-with-values`.
- [x] Exceptions: `error`, `raise`, `guard`.
- [ ] Continuations: `call/cc`, `dynamic-wind`.

### Core Forms
- [x] `case`.
- [x] `do`.
- [x] `delay` / `force`.

### Data Types And Procedures
- [x] Broader symbol procedures: `symbol->string`, `string->symbol`.
- [x] Broader character procedures: comparisons, ordering, case predicates, conversions.
- [ ] Broader string procedures.
- [ ] Broader vector procedures.
- [ ] Bytevectors.
- [ ] Stronger numeric tower coverage beyond tagged fixnums.

### List Library
- [x] `map`.
- [x] `for-each`.
- [x] `memq`, `memv`, `member`.
- [x] `assq`, `assv`, `assoc`.

### I/O
- [ ] Input ports.
- [ ] Output ports beyond current stdout helpers.
- [ ] `read`.
- [ ] File procedures.

### Libraries And Macros
- [x] `define-library`.
- [x] `import`.
- [x] `define-syntax`.
- [ ] `syntax-rules`.

## Runtime And Compiler Work

### GC Correctness
- [ ] Make GC root/liveness handling systematic in normal codegen, not case-by-case.
- [ ] Extend rooted-value handling beyond current pair-mutation/local-binding fixes.
- [ ] Keep ordinary Scheme codegen correct across all GC-visible runtime calls.

### LLVM / Statepoints
- [ ] Move normal Scheme backend closer to explicit statepoint/relocate-aware lowering.
- [ ] Reduce reliance on ad hoc backend paths where rooted stack slots are a better fit.
- [ ] Decide whether to add a thin `llvm-sys` statepoint builder for direct emission.

### Calling Convention
- [ ] Improve dynamic procedure calling coverage.
- [ ] Finish `apply` for all first-class procedures, not just known-signature user procedures/closures.
- [ ] Support variadic and `apply`-driven call lowering.
- [ ] Keep closure/runtime calling conventions consistent across more dynamic cases.

## Test Expansion
- [ ] Add more e2e Scheme cases for lists, strings, symbols, chars, vectors, and closures.
- [ ] Add stronger GC stress coverage across more object shapes.
- [ ] Add more multithreaded runtime/compiler integration tests.

## Recommended Next Order
1. [ ] Continuations: `call/cc`, `dynamic-wind`
2. [ ] Broader strings, vectors, bytevectors, and numeric coverage
