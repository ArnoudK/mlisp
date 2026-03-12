use mlisp::backend::LlvmBackend;
use mlisp::frontend::parse_program;
use mlisp::middle::lower_program;

#[test]
fn parses_and_lowers_define() {
    let ast = parse_program("(define answer 42)\n(+ answer 1)\n").unwrap();
    assert_eq!(ast.forms.len(), 2);

    let hir = lower_program(&ast).unwrap();
    assert_eq!(hir.items.len(), 2);
}

#[test]
fn lowers_r7rs_control_forms_to_core_hir() {
    let ast = parse_program(
        "(begin (and 1 2) (or #f 3) (when #t 1) (unless #f 2) (cond ((#f) 0) (else 1)))\n",
    )
    .unwrap();
    let hir = lower_program(&ast).unwrap();
    let rendered = format!("{hir:#?}");

    assert!(rendered.contains("Let"));
    assert!(rendered.contains("If"));
    assert!(rendered.contains("Unspecified"));
}

#[test]
fn lowers_letrec_star_to_nested_letrec() {
    let ast = parse_program("(letrec* ((f (lambda () 1)) (g (lambda () (f)))) (g))\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let rendered = format!("{hir:#?}");

    assert!(rendered.matches("LetRec").count() >= 2);
}

#[test]
fn emits_addition_ir() {
    let ast = parse_program("(define answer 41)\n(+ answer 1)\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("test_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("define i64 @main"));
    assert!(compiled.llvm_ir.contains("ret i64 85"));
}

#[test]
fn emits_procedure_definition_and_call() {
    let ast = parse_program("(define (add2 x) (+ x 2))\n(add2 40)\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("proc_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("define i64 @add2(i64 %0)"));
    assert!(compiled.llvm_ir.contains("call i64 @add2(i64 81)"));
}

#[test]
fn emits_heap_returning_procedure_with_pointer_signature() {
    let ast = parse_program("(define (mkpair) (cons 1 2))\n(car (mkpair))\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("heap_proc_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("define ptr addrspace(1) @mkpair()"));
    assert!(compiled.llvm_ir.contains("call ptr addrspace(1) @mkpair()"));
    assert!(compiled.llvm_ir.contains("call i64 @__mlisp_pair_car_gc_as1(ptr addrspace(1)"));
}

#[test]
fn emits_heap_parameter_procedure_with_pointer_signature() {
    let ast = parse_program("(define (pair-head p) (car p))\n(pair-head (cons 1 2))\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("heap_param_proc_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("define i64 @pair-head(ptr addrspace(1) %0)"));
    assert!(compiled.llvm_ir.contains("call i64 @pair-head(ptr addrspace(1)"));
    assert!(compiled.llvm_ir.contains("call i64 @__mlisp_pair_car_gc_as1(ptr addrspace(1) %0)"));
}

#[test]
fn emits_direct_lambda_application() {
    let ast = parse_program("((lambda (x) (+ x 1)) 41)\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("lambda_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("define i64 @__lambda_0(i64 %0)"));
    assert!(compiled.llvm_ir.contains("call i64 @__lambda_0(i64 83)"));
}

#[test]
fn emits_heap_returning_lambda_with_pointer_signature() {
    let ast = parse_program("(vector-ref ((lambda () (vector 1 2 3))) 1)\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("heap_lambda_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("define ptr addrspace(1) @__lambda_0()"));
    assert!(compiled.llvm_ir.contains("call ptr addrspace(1) @__lambda_0()"));
    assert!(compiled.llvm_ir.contains("call i64 @__mlisp_vector_ref_gc_as1(ptr addrspace(1)"));
}

#[test]
fn emits_heap_parameter_lambda_with_pointer_signature() {
    let ast = parse_program("((lambda (v) (vector-ref v 1)) (vector 1 2 3))\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("heap_param_lambda_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("define i64 @__lambda_0(ptr addrspace(1) %0)"));
    assert!(compiled.llvm_ir.contains("call i64 @__lambda_0(ptr addrspace(1)"));
    assert!(compiled.llvm_ir.contains("call i64 @__mlisp_vector_ref_gc_as1(ptr addrspace(1) %0"));
}

#[test]
fn emits_captured_closure_allocation_and_indirect_call() {
    let ast = parse_program("((let ((x 41)) (lambda () (+ x 1))))\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("captured_closure_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("define i64 @__lambda_0(ptr addrspace(1) %0)"));
    assert!(compiled.llvm_ir.contains("call ptr addrspace(1) @__mlisp_alloc_closure_gc_as1"));
    assert!(compiled.llvm_ir.contains("%closure.code = load i64"));
    assert!(compiled.llvm_ir.contains("call i64 %closure.fn(ptr addrspace(1)"));
}

#[test]
fn emits_captured_heap_value_through_closure_env() {
    let ast = parse_program("(car ((let ((p (cons 1 2))) (lambda () p))))\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("captured_pair_closure_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("%closure.capture.0 = load i64"));
    assert!(compiled.llvm_ir.contains("inttoptr i64 %closure.capture.0 to ptr addrspace(1)"));
    assert!(compiled.llvm_ir.contains("call i64 @__mlisp_pair_car_gc_as1(ptr addrspace(1)"));
}

#[test]
fn lowers_let_and_let_star() {
    let parallel = parse_program("(let ((x 1) (y 2)) (+ x y))\n").unwrap();
    let sequential = parse_program("(let* ((x 1) (y (+ x 2))) (+ x y))\n").unwrap();

    let parallel_hir = lower_program(&parallel).unwrap();
    let sequential_hir = lower_program(&sequential).unwrap();

    let parallel_ir = LlvmBackend::compile_program("let_module", &parallel_hir).unwrap();
    let sequential_ir = LlvmBackend::compile_program("let_star_module", &sequential_hir).unwrap();

    assert!(parallel_ir.llvm_ir.contains("ret i64 7"));
    assert!(sequential_ir.llvm_ir.contains("ret i64 9"));
}

#[test]
fn emits_letrec_local_recursive_procedure() {
    let source = "\
        (letrec ((countdown
                    (lambda (n)
                      (if n (countdown (- n 1)) 0))))
          (countdown 3))
    ";
    let ast = parse_program(source).unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("letrec_module", &hir).unwrap();

    assert!(compiled
        .llvm_ir
        .contains("define i64 @__letrec_countdown_0(ptr addrspace(1) %0, i64 %1)"));
    assert!(compiled.llvm_ir.contains("call ptr addrspace(1) @__mlisp_alloc_closure_gc_as1"));
    assert!(compiled.llvm_ir.contains("%closure.code = load i64"));
}

#[test]
fn emits_letrec_recursive_closure_with_outer_capture() {
    let source = "\
        (let ((x 9))
          (letrec ((countdown
                      (lambda (n)
                        (if n (countdown (- n 1)) x))))
            (countdown 3)))
    ";
    let ast = parse_program(source).unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("letrec_closure_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("call ptr addrspace(1) @__mlisp_alloc_closure_gc_as1"));
    assert!(compiled.llvm_ir.contains("call i64 @__mlisp_closure_env_set_gc_as1"));
    assert!(compiled.llvm_ir.contains("%closure.code = load i64"));
}

#[test]
fn compiles_letrec_star_with_sequential_recursive_scope() {
    let source = "\
        (letrec* ((f (lambda () 1))
                  (g (lambda () (f))))
          (g))
    ";
    let ast = parse_program(source).unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("letrec_star_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("@__mlisp_alloc_closure_gc_as1"));
    assert!(compiled.llvm_ir.contains("call i64 %closure.fn"));
}

#[test]
fn allocates_pairs_on_heap_but_keeps_fixnums_immediate() {
    let ast = parse_program("(car (cons 1 2))\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("pair_module", &hir).unwrap();

    assert!(
        compiled
            .llvm_ir
            .contains("call ptr addrspace(1) @__mlisp_alloc_pair_gc_as1(i64 3, i64 5)")
    );
    assert!(compiled.llvm_ir.contains("call i64 @__mlisp_pair_car_gc_as1"));
}

#[test]
fn supports_pair_and_null_predicates() {
    let pair_ast = parse_program("(pair? (cons 1 2))\n").unwrap();
    let null_ast = parse_program("(null? '())\n").unwrap();

    let pair_hir = lower_program(&pair_ast).unwrap();
    let null_hir = lower_program(&null_ast).unwrap();

    let pair_ir = LlvmBackend::compile_program("pair_predicate_module", &pair_hir).unwrap();
    let null_ir = LlvmBackend::compile_program("null_predicate_module", &null_hir).unwrap();

    assert!(
        pair_ir
            .llvm_ir
            .contains("call ptr addrspace(1) @__mlisp_alloc_pair_gc_as1(i64 3, i64 5)")
    );
    assert!(pair_ir.llvm_ir.contains("ret i64 6"));
    assert!(null_ir.llvm_ir.contains("ret i64 6"));
}

#[test]
fn inserts_fixnum_guard_for_numeric_operands() {
    let ast = parse_program("(define (add1 x) (+ x 1))\n(add1 41)\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("numeric_guard_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("llvm.trap"));
    assert!(compiled.llvm_ir.contains("arg0.fixnum.check.is_fixnum"));
}

#[test]
fn compiles_zero_predicate() {
    let ast = parse_program("(zero? 0)\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("zero_predicate_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("ret i64 6"));
}

#[test]
fn compiles_and_or_when_unless_and_cond_forms() {
    let source = "\
        (begin
          (and 1 2)
          (or #f 3)
          (when #t 4)
          (unless #f 5)
          (cond ((zero? 1) 6) ((zero? 0) 7) (else 8)))
    ";
    let ast = parse_program(source).unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("control_forms_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("br i1"));
}

#[test]
fn lowers_and_compiles_set_bang_with_mutable_binding_boxes() {
    let ast = parse_program("(let ((x 1)) (begin (set! x 2) x))\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("set_box_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("@__mlisp_alloc_box_gc_as1"));
    assert!(compiled.llvm_ir.contains("@__mlisp_box_set_gc_as1"));
}

#[test]
fn compiles_set_bang_shared_with_closure_capture() {
    let source = "\
        (let ((x 1))
          (let ((f (lambda () x)))
            (begin
              (set! x 2)
              (f))))
    ";
    let ast = parse_program(source).unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("set_closure_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("@__mlisp_alloc_box_gc_as1"));
    assert!(compiled.llvm_ir.contains("@__mlisp_box_set_gc_as1"));
    assert!(compiled.llvm_ir.contains("closure.capture.0.box"));
}

#[test]
fn rejects_integer_literals_outside_fixnum_range() {
    let source = format!("{}\n", i64::MAX);
    let ast = parse_program(&source).unwrap();
    let hir = lower_program(&ast).unwrap();
    let error = LlvmBackend::compile_program("overflow_literal_module", &hir).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("does not fit in the tagged fixnum representation")
    );
}

#[test]
fn merges_pair_values_across_if_branches() {
    let ast = parse_program("(car (if #t (cons 1 2) (cons 3 4)))\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("if_pair_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("phi ptr addrspace(1)"));
    assert!(compiled.llvm_ir.contains("call i64 @__mlisp_pair_car_gc_as1(ptr addrspace(1)"));
}

#[test]
fn rejects_if_branches_with_mismatched_value_kinds() {
    let ast = parse_program("(if #t 1 (cons 2 3))\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let error = LlvmBackend::compile_program("if_mismatch_module", &hir).unwrap_err();

    assert!(error.to_string().contains("if branches must produce the same kind of value"));
}

#[test]
fn compiles_string_literals_and_accessors() {
    let ast = parse_program("(string-ref \"hello\" 1)\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("string_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("@mlisp_alloc_string"));
    assert!(compiled.llvm_ir.contains("@mlisp_string_ref"));
    assert!(compiled.llvm_ir.contains("c\"hello\""));
}

#[test]
fn compiles_quoted_symbol_and_list_data() {
    let ast = parse_program("(begin 'hello '(1 hello \"x\"))\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("quote_data_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("@__mlisp_alloc_symbol_gc_as1"));
    assert!(compiled.llvm_ir.contains("@__mlisp_alloc_pair_gc_as1"));
}

#[test]
fn compiles_character_literals_and_predicate() {
    let ast = parse_program("(char? #\\x)\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("char_module", &hir).unwrap();

    assert!(!compiled.llvm_ir.contains("__string_literal_"));
    assert!(compiled.llvm_ir.contains("ret i64 6"));
}

#[test]
fn compiles_eq_and_eqv_for_words_and_heap_refs() {
    let immediate_ast = parse_program("(eq? #\\x #\\x)\n").unwrap();
    let heap_ast = parse_program("(let ((p (cons 1 2))) (eqv? p p))\n").unwrap();
    let immediate_hir = lower_program(&immediate_ast).unwrap();
    let heap_hir = lower_program(&heap_ast).unwrap();
    let immediate_ir =
        LlvmBackend::compile_program("eq_immediate_module", &immediate_hir).unwrap();
    let heap_ir = LlvmBackend::compile_program("eq_heap_module", &heap_hir).unwrap();

    assert!(immediate_ir.llvm_ir.contains("ret i64 6"));
    assert!(heap_ir.llvm_ir.contains("ptrtoint ptr addrspace(1)"));
    assert!(heap_ir.llvm_ir.contains("icmp eq i64"));
}

#[test]
fn compiles_not_and_list_builtins() {
    let not_ast = parse_program("(not #f)\n").unwrap();
    let list_ast = parse_program("(length (list 1 2 3))\n").unwrap();
    let not_hir = lower_program(&not_ast).unwrap();
    let list_hir = lower_program(&list_ast).unwrap();
    let not_ir = LlvmBackend::compile_program("not_module", &not_hir).unwrap();
    let list_ir = LlvmBackend::compile_program("list_module", &list_hir).unwrap();

    assert!(not_ir.llvm_ir.contains("ret i64 6"));
    assert!(list_ir.llvm_ir.contains("@__mlisp_alloc_pair_gc_as1"));
    assert!(list_ir.llvm_ir.contains("@mlisp_list_length"));
}

#[test]
fn compiles_symbol_boolean_and_procedure_predicates() {
    let symbol_ast = parse_program("(symbol? 'hello)\n").unwrap();
    let boolean_ast = parse_program("(boolean? #t)\n").unwrap();
    let procedure_ast = parse_program("(procedure? (lambda (x) x))\n").unwrap();
    let symbol_hir = lower_program(&symbol_ast).unwrap();
    let boolean_hir = lower_program(&boolean_ast).unwrap();
    let procedure_hir = lower_program(&procedure_ast).unwrap();
    let symbol_ir = LlvmBackend::compile_program("symbol_pred_module", &symbol_hir).unwrap();
    let boolean_ir = LlvmBackend::compile_program("boolean_pred_module", &boolean_hir).unwrap();
    let procedure_ir =
        LlvmBackend::compile_program("procedure_pred_module", &procedure_hir).unwrap();

    assert!(symbol_ir.llvm_ir.contains("@mlisp_is_symbol"));
    assert!(boolean_ir.llvm_ir.contains("ret i64 6"));
    assert!(procedure_ir.llvm_ir.contains("ret i64 6"));
}

#[test]
fn compiles_list_ref_tail_and_append() {
    let ref_ast = parse_program("(list-ref (list 1 2 3) 1)\n").unwrap();
    let tail_ast = parse_program("(list-tail (list 1 2 3) 1)\n").unwrap();
    let append_ast = parse_program("(append (list 1 2) (list 3))\n").unwrap();
    let ref_hir = lower_program(&ref_ast).unwrap();
    let tail_hir = lower_program(&tail_ast).unwrap();
    let append_hir = lower_program(&append_ast).unwrap();
    let ref_ir = LlvmBackend::compile_program("list_ref_module", &ref_hir).unwrap();
    let tail_ir = LlvmBackend::compile_program("list_tail_module", &tail_hir).unwrap();
    let append_ir = LlvmBackend::compile_program("append_module", &append_hir).unwrap();

    assert!(ref_ir.llvm_ir.contains("@mlisp_list_ref"));
    assert!(tail_ir.llvm_ir.contains("@mlisp_list_tail"));
    assert!(append_ir.llvm_ir.contains("@mlisp_append"));
}

#[test]
fn supports_string_predicate_and_length() {
    let predicate_ast = parse_program("(string? \"hi\")\n").unwrap();
    let length_ast = parse_program("(string-length \"hi\")\n").unwrap();
    let predicate_hir = lower_program(&predicate_ast).unwrap();
    let length_hir = lower_program(&length_ast).unwrap();
    let predicate_ir =
        LlvmBackend::compile_program("string_predicate_module", &predicate_hir).unwrap();
    let length_ir = LlvmBackend::compile_program("string_length_module", &length_hir).unwrap();

    assert!(predicate_ir.llvm_ir.contains("@mlisp_alloc_string"));
    assert!(length_ir.llvm_ir.contains("@mlisp_string_length"));
}

#[test]
fn compiles_display_builtin() {
    let ast = parse_program("(display \"hi\")\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("display_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("@mlisp_display"));
}

#[test]
fn compiles_write_and_newline_builtins() {
    let ast = parse_program("(begin (write \"hi\") (newline))\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("write_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("@mlisp_write"));
    assert!(compiled.llvm_ir.contains("@mlisp_newline"));
}

#[test]
fn compiles_vector_construction_and_access() {
    let ast = parse_program("(vector-ref (vector 1 2 3) 1)\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("vector_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("@mlisp_alloc_vector"));
    assert!(compiled.llvm_ir.contains("@mlisp_vector_ref"));
}

#[test]
fn compiles_vector_mutation_through_runtime_barrier() {
    let ast = parse_program("(let ((v (vector 1 2))) (begin (vector-set! v 1 9) (vector-ref v 1)))\n")
        .unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("vector_set_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("@mlisp_vector_set"));
    assert!(compiled.llvm_ir.contains("@mlisp_vector_ref"));
}
