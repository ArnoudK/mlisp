# TODO

## R7RS Small Gaps

### Core Semantics
- [ ] Proper tail recursion as a real guarantee.
- [ ] `apply`.
- [x] Variadic procedures.
- [x] Dotted parameter lists.
- [ ] `equal?`.
- [ ] Multiple values: `values`, `call-with-values`.
- [ ] Exceptions: `error`, `raise`, `guard`.
- [ ] Continuations: `call/cc`, `dynamic-wind`.

### Core Forms
- [ ] `case`.
- [ ] `do`.
- [ ] `delay` / `force`.

### Data Types And Procedures
- [ ] Broader symbol procedures: `symbol->string`, `string->symbol`.
- [ ] Broader character procedures: comparisons, ordering, case predicates, conversions.
- [ ] Broader string procedures.
- [ ] Broader vector procedures.
- [ ] Bytevectors.
- [ ] Stronger numeric tower coverage beyond tagged fixnums.

### List Library
- [ ] `map`.
- [ ] `for-each`.
- [ ] `memq`, `memv`, `member`.
- [ ] `assq`, `assv`, `assoc`.

### I/O
- [ ] Input ports.
- [ ] Output ports beyond current stdout helpers.
- [ ] `read`.
- [ ] File procedures.

### Libraries And Macros
- [ ] `define-library`.
- [ ] `import`.
- [ ] `define-syntax`.
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
- [ ] Support variadic and `apply`-driven call lowering.
- [ ] Keep closure/runtime calling conventions consistent across more dynamic cases.

## Test Expansion
- [ ] Add more e2e Scheme cases for lists, strings, symbols, chars, vectors, and closures.
- [ ] Add stronger GC stress coverage across more object shapes.
- [ ] Add more multithreaded runtime/compiler integration tests.

## Recommended Next Order
1. [ ] `apply`
2. [ ] Variadic procedures and dotted parameters
3. [ ] `equal?`
4. [ ] Symbol and character procedure families
5. [ ] `map`, `for-each`, and member/assoc procedures
6. [ ] Proper tail recursion
7. [ ] Macros and libraries
