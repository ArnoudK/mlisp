# mlisp
A small project to test codex. it compiles scheme to llvm using mmtk for GC. 

Current direction:

- R7RS-inspired Lisp frontend with source-aware lexing, parsing, and lowering.
- LLVM IR generation through `inkwell` targeting the locally installed LLVM 21 toolchain.
- Multi-threaded compilation at the compilation-unit level: each source file is parsed, lowered, and code-generated on its own worker thread with an isolated LLVM context.

Current implemented subset:

- Integers
- Booleans
- Symbols
- Quoted data in the frontend and HIR
- Top-level `define`
- Top-level procedure definitions in the R7RS small shorthand form
- `begin`
- `if`
- Non-capturing `lambda` code generation and direct lambda application
- `let`, `let*`, and `letrec` for local lexical bindings
- Arithmetic forms `+`, `-`, `*`, `/` in the LLVM backend
- Direct calls to compiled top-level procedures
- Tagged Scheme-word code generation for immediate values instead of raw host integers

Quick start:

```bash
cargo run -- compile examples/hello.scm --print-ir --emit-ir-dir target/ir
```

To emit the current compiler-generated pre-statepoint GC example:

```bash
cargo run -- gc-example
```

To run the proper LLVM safepoint rewrite pipeline on compiler-generated IR:

```bash
cargo run -- gc-pipeline-example
```

To exercise the MMTk runtime prototype with StickyImmix and bound mutator threads:

```bash
cargo run -- runtime-stress --threads 2 --iterations 256
```

Runtime direction:

- Tagged `Value` representation for immediates and heap references
- Heap object model prepared for a moving collector
- Separate `runtime/` crate implemented in Rust and linked into the compiler workspace
- MMTk binding using StickyImmix with concrete `ActivePlan`, `Collection`, `Scanning`, `ObjectModel`, and `ReferenceGlue` implementations
- Runtime ABI surface including `rt_mmtk_init`, `rt_bind_thread`, `rt_unbind_thread`, `rt_alloc_slow`, `rt_gc_poll`, and `rt_object_write_post`
- Compiler-side safepoint examples now declare and exercise the real `rt_*` runtime symbols

See [docs/runtime.md](/home/du/code/mlisp/docs/runtime.md) for the current memory-management plan.
See [docs/statepoints.md](/home/du/code/mlisp/docs/statepoints.md) for the planned LLVM safepoint model.

The `r7rs/` directory remains in-tree as a language reference and source of future implementation work.
