# Compiler Refactor Plan

## Why

The current compiler flow is still a single-shot CLI pipeline:

`read file -> parse -> expand -> lower -> codegen -> link -> execute`

That works for batch compilation, but it is the wrong shape for:

- stage-only execution for editor/LSP use
- precise source-based diagnostics across AST, expansion, HIR, and codegen
- REPL-style incremental evaluation

The main coupling point today is [`src/driver.rs`](/home/du/code/mlisp/src/driver.rs), which owns parsing, expansion, lowering, codegen, linking, and execution in one place. Frontend AST nodes already carry a raw byte `Span` in [`src/frontend/ast.rs`](/home/du/code/mlisp/src/frontend/ast.rs), but HIR and most diagnostics drop that information.

## Refactor Goals

- Make the compiler usable as a long-lived service, not only a one-shot CLI.
- Expose each compiler stage as an explicit API with typed inputs and outputs.
- Preserve source origin for every meaningful AST/HIR/codegen node.
- Support incremental and interactive workflows needed by LSP and REPL.
- Keep the CLI as a thin client over the same service API.

## Non-Goals

- Do not redesign the language semantics as part of this refactor.
- Do not make LLVM codegen incremental in the first pass unless it blocks REPL.
- Do not add LSP itself in this refactor; build the compiler shape that LSP can call.

## Target Architecture

Move toward this split:

- `compiler core`
  - pure stage functions
  - stage artifact types
  - source maps / spans / diagnostics
- `compiler service`
  - long-lived state
  - file/session management
  - stage orchestration
  - caching / invalidation
- `clients`
  - CLI
  - future LSP
  - REPL

Suggested high-level APIs:

- `parse(source_id, text) -> ParsedUnit`
- `expand(parsed, environment) -> ExpandedUnit`
- `lower(expanded) -> HirUnit`
- `codegen(hir) -> CompiledModule`
- `link(module_set) -> ExecutableArtifact`
- `execute(executable | jit unit, runtime_session) -> ExecutionResult`

Suggested service concepts:

- `CompilerService`
- `CompilationSession`
- `SourceManager`
- `Diagnostic`
- `StageRequest`
- `StageResult<T>`

## Main Design Decisions To Make Early

- Choose whether stage APIs operate on immutable artifacts or mutable session-owned state.
- Decide whether the REPL uses LLVM module accumulation, top-level interpretation, or compile-and-run per entry.
- Decide whether macro expansion environments are session-scoped, file-scoped, or both.
- Decide whether source locations are stored inline on every node or via node IDs plus side tables.
- Decide whether diagnostics are recoverable per stage or still fail-fast.

## Phase 0: Baseline And Invariants

- [ ] Document the current pipeline and ownership boundaries in code comments or docs.
- [ ] List all public entrypoints that currently bypass a staged API:
  - [ ] `compile_paths`
  - [ ] `run_path`
  - [ ] `compile_gc_example`
  - [ ] `compile_gc_pipeline_example`
- [ ] Define compatibility expectations for existing CLI behavior and tests.
- [ ] Add characterization tests for the current pipeline before moving code.
- [ ] Add golden tests for current parse/expand/lower/codegen outputs where practical.

## Phase 1: Introduce Compiler Artifacts And Stage APIs

- [ ] Create explicit artifact types instead of passing raw source/AST/HIR ad hoc.
- [ ] Introduce a source identity type:
  - [ ] `SourceId`
  - [ ] source path
  - [ ] source text
  - [ ] version/revision
- [ ] Introduce stage result types:
  - [ ] `ParsedUnit`
  - [ ] `ExpandedUnit`
  - [ ] `LoweredUnit`
  - [ ] `CodegenUnit`
- [ ] Move stage functions out of the CLI driver path into a compiler-core module.
- [ ] Refactor `src/driver.rs` to call stage APIs instead of hardcoding the pipeline.
- [ ] Ensure each stage has a narrow contract:
  - [ ] required inputs
  - [ ] produced artifact
  - [ ] diagnostics
  - [ ] dependencies on session/environment
- [ ] Keep old CLI behavior passing through the new APIs.

## Phase 2: Source Management And Diagnostics Foundation

- [ ] Replace bare byte offsets with a richer source model.
- [ ] Extend [`src/span.rs`](/home/du/code/mlisp/src/span.rs) to represent:
  - [ ] `SourceId`
  - [ ] start byte
  - [ ] end byte
  - [ ] optional expansion origin / parent span
- [ ] Add a `SourceManager` that can map byte spans to:
  - [ ] line/column
  - [ ] source snippet
  - [ ] file path / virtual source name
- [ ] Introduce a structured `Diagnostic` type instead of plain strings in `CompileError`.
- [ ] Support:
  - [ ] primary span
  - [ ] secondary spans
  - [ ] severity
  - [ ] stage
  - [ ] message
  - [ ] notes/help text
- [ ] Make `CompileError` either wrap diagnostics or become a transport for fatal internal failures only.
- [ ] Add formatter(s) for CLI-friendly diagnostics.

## Phase 3: Preserve Origins Through AST, Expansion, And HIR

- [ ] Audit every AST constructor in parser and expander to ensure spans are preserved or intentionally synthesized.
- [ ] Introduce explicit origin tracking for macro-generated forms.
- [ ] Decide and implement one of:
  - [ ] inline `NodeOrigin` on every node
  - [ ] stable node IDs with side-table origins
- [ ] Extend HIR nodes in [`src/middle/hir.rs`](/home/du/code/mlisp/src/middle/hir.rs) to carry origins.
- [ ] Update lowering in [`src/middle/lowering.rs`](/home/du/code/mlisp/src/middle/lowering.rs) to preserve origin information on:
  - [ ] top-level forms
  - [ ] expressions
  - [ ] bindings
  - [ ] procedures
  - [ ] quoted data where relevant
- [ ] For lowered desugarings (`and`, `or`, `cond`, `case`, `when`, `do`, etc.), preserve both:
  - [ ] user-facing source origin
  - [ ] synthetic-node marker
- [ ] Add tests proving that diagnostics can point back to original user code after expansion/lowering.

## Phase 4: Split Orchestration From Execution

- [ ] Separate compilation stages from native linking/execution concerns.
- [ ] Move binary build/link logic out of the core pipeline path.
- [ ] Introduce execution artifacts:
  - [ ] object/module IR artifact
  - [ ] linked executable artifact
  - [ ] runtime execution result
- [ ] Make `run` be an explicit client flow:
  - [ ] compile requested stage(s)
  - [ ] link if needed
  - [ ] execute if needed
- [ ] Ensure compile-only workflows never require runtime initialization or linking.
- [ ] Keep runtime-specific helpers isolated from the pure compiler service interface.

## Phase 5: Compiler Service / Session Model

- [ ] Introduce `CompilerService` as the long-lived owner of shared compiler infrastructure.
- [ ] Introduce `CompilationSession` for one logical client interaction or project state.
- [ ] Move file loading out of stage functions; stages should consume source artifacts, not paths.
- [ ] Support in-memory sources so LSP/REPL do not need on-disk files.
- [ ] Add session-owned environment state for:
  - [ ] loaded libraries
  - [ ] macro definitions
  - [ ] symbol/value environment
  - [ ] source table
  - [ ] cached stage artifacts
- [ ] Define invalidation rules:
  - [ ] file content changed
  - [ ] import dependency changed
  - [ ] macro environment changed
  - [ ] runtime/repl top-level changed
- [ ] Add dependency tracking between source units and imported libraries.

## Phase 6: Stage-Only Execution

- [ ] Define a user-facing stage enum, for example:
  - [ ] `Lex`
  - [ ] `Parse`
  - [ ] `Expand`
  - [ ] `Lower`
  - [ ] `Codegen`
- [ ] Expose service methods to request execution up to a specific stage.
- [ ] Ensure every stage can return partial success plus diagnostics where appropriate.
- [ ] Add serialization/debug rendering for artifacts so clients can inspect outputs.
- [ ] Extend CLI with stage-only commands or flags, for example:
  - [ ] `compile --stage parse`
  - [ ] `compile --stage expand`
  - [ ] `compile --stage lower`
  - [ ] `compile --stage codegen`
- [ ] Add tests for:
  - [ ] stage selection
  - [ ] no unnecessary downstream work
  - [ ] diagnostics without codegen/linking
- [ ] Make sure LSP-oriented requests can operate on unsaved in-memory text.

## Phase 7: REPL Infrastructure

- [ ] Decide the REPL execution strategy:
  - [ ] compile-and-run each form in an evolving session
  - [ ] accumulate a module and re-link
  - [ ] hybrid strategy
- [ ] Add a `ReplSession` built on top of `CompilationSession`.
- [ ] Support incremental top-level state across inputs:
  - [ ] variable definitions
  - [ ] procedure definitions
  - [ ] macro definitions
  - [ ] imported libraries
- [ ] Support reading one form at a time instead of requiring whole files.
- [ ] Add result printing policy for values and exceptions.
- [ ] Add recoverable diagnostics so one bad form does not kill the session.
- [ ] Handle incomplete input cleanly:
  - [ ] unclosed parens
  - [ ] unterminated strings
  - [ ] multiline forms
- [ ] Decide whether REPL code uses virtual source names like `<repl:line-12>`.
- [ ] Add a CLI command for REPL that uses the same service/session APIs.
- [ ] Add tests for:
  - [ ] persistent bindings across entries
  - [ ] persistent macros across entries
  - [ ] imports across entries
  - [ ] errors with correct source locations
  - [ ] multiline input handling

## Phase 8: Backend Source-Aware Errors

- [ ] Thread origin information far enough into codegen to blame user source on backend failures.
- [ ] Audit LLVM/codegen error sites in [`src/backend/llvm.rs`](/home/du/code/mlisp/src/backend/llvm.rs).
- [ ] Replace generic backend string errors with diagnostics that attach source origins when available.
- [ ] Add source-aware checks for:
  - [ ] duplicate definitions
  - [ ] invalid call shapes
  - [ ] unsupported constructs
  - [ ] arity mismatches where statically known
- [ ] Preserve a clear distinction between:
  - [ ] user program errors
  - [ ] compiler bugs/internal errors
  - [ ] external tool failures such as `clang`

## Phase 9: CLI Migration

- [ ] Refactor the CLI into a thin transport over the compiler service.
- [ ] Keep current commands working during migration.
- [ ] Add new commands/flags:
  - [ ] stage selection
  - [ ] artifact printing/dumping
  - [ ] REPL entrypoint
  - [ ] diagnostic format selection
- [ ] Ensure no CLI command directly reads/parses/lowers/codegens outside the service API.
- [ ] Update README and usage docs.

## Phase 10: Testing And Validation

- [ ] Add unit tests for each stage artifact type and transition.
- [ ] Add span propagation tests from lexer -> parser -> expander -> lowerer.
- [ ] Add diagnostic rendering tests with line/column/snippet output.
- [ ] Add stage-only integration tests.
- [ ] Add REPL transcript tests.
- [ ] Add in-memory source tests for future editor integration.
- [ ] Add regression tests for import/macro invalidation.
- [ ] Keep existing end-to-end execution tests green throughout the migration.

## Suggested Implementation Order

- [ ] Start with artifacts + diagnostics, not REPL.
- [ ] Then make stages independently callable.
- [ ] Then introduce service/session state.
- [ ] Then build REPL on top of the service.
- [ ] Only after that, add a richer CLI surface for the new capabilities.

## Concrete Code Moves

- [ ] Create a new `compiler` or `service` module tree instead of growing `driver.rs`.
- [ ] Move path-based file IO behind a source-loading layer.
- [ ] Keep `frontend`, `expand`, `middle`, and `backend` mostly focused on transformations.
- [ ] Move process execution and linking concerns out of compiler-core.
- [ ] Reserve `driver.rs` for CLI parsing and request translation only.

## Risks

- [ ] Macro expansion origin tracking can get messy if synthetic forms overwrite user spans.
- [ ] REPL persistence may force earlier decisions about environment representation than expected.
- [ ] Backend APIs may need larger signature changes once HIR carries origin metadata.
- [ ] Test updates may be broad because many current helpers assume file-based one-shot compilation.

## Definition Of Done

- [ ] CLI uses the compiler service instead of owning the pipeline directly.
- [ ] Any stage can be invoked independently on file-backed or in-memory source.
- [ ] Diagnostics can report file, line, column, and source snippet.
- [ ] HIR retains source origin information.
- [ ] REPL works with persistent definitions/imports/macros across inputs.
- [ ] Existing batch compile/run flows still work.
