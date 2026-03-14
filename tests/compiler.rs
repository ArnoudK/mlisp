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
fn lowers_case_and_do_to_core_hir() {
    let ast = parse_program(
        "(begin (case 2 ((1) 0) ((2 3) 4) (else 5)) (do ((i 0 (+ i 1)) (acc 0 (+ acc i))) ((zero? (- 4 i)) acc)))\n",
    )
    .unwrap();
    let hir = lower_program(&ast).unwrap();
    let rendered = format!("{hir:#?}");

    assert!(rendered.contains("\"memv\""));
    assert!(rendered.contains("LetRec"));
    assert!(rendered.contains("Lambda"));
}

#[test]
fn lowers_delay_and_force() {
    let ast = parse_program("(begin (delay (+ 1 2)) (force (delay 4)))\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let rendered = format!("{hir:#?}");

    assert!(rendered.contains("Delay"));
    assert!(rendered.contains("Force"));
}

#[test]
fn compiles_delay_and_force_promises() {
    let ast = parse_program("(let ((p (delay (cons 1 2)))) (car (force p)))\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("promise_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("@__mlisp_alloc_promise_gc_as1"));
    assert!(compiled.llvm_ir.contains("@__mlisp_promise_forced_gc_as1"));
    assert!(compiled.llvm_ir.contains("@__mlisp_promise_resolve_gc_as1"));
}

#[test]
fn lowers_letrec_star_to_nested_letrec() {
    let ast = parse_program("(letrec* ((f (lambda () 1)) (g (lambda () (f)))) (g))\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let rendered = format!("{hir:#?}");

    assert!(rendered.matches("LetRec").count() >= 2);
}

#[test]
fn parses_dotted_quote_and_variadic_formals() {
    let ast = parse_program("'(1 . 2)\n(lambda (x y . rest) rest)\n(lambda args args)\n").unwrap();
    let rendered = format!("{ast:#?}");

    assert!(rendered.contains("tail: Some"));
    assert!(rendered.contains("\"rest\""));
    assert!(rendered.contains("\"args\""));
}

#[test]
fn lowers_variadic_lambda_and_dotted_quote() {
    let ast = parse_program("((lambda (x . rest) rest) 1 2 3)\n'(1 . 2)\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let rendered = format!("{hir:#?}");

    assert!(rendered.contains("rest: Some("));
    assert!(rendered.contains("Datum::List") || rendered.contains("List {"));
    assert!(rendered.contains("tail: Some"));
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
fn emits_variadic_procedure_with_rest_list_parameter() {
    let ast = parse_program("(define (tail x . rest) rest)\n(tail 1 2 3)\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("variadic_proc_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("define i64 @tail(i64 %0, i64 %1)"));
    assert!(compiled.llvm_ir.contains("call ptr addrspace(1) @__mlisp_alloc_pair_gc_as1"));
    assert!(compiled.llvm_ir.contains("call i64 @tail(i64 3, i64"));
}

#[test]
fn emits_variadic_lambda_with_rest_list_parameter() {
    let ast = parse_program("((lambda (x y . rest) rest) 1 2 3 4)\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("variadic_lambda_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("define i64 @__lambda_0(i64 %0, i64 %1, i64 %2)"));
    assert!(compiled.llvm_ir.contains("call ptr addrspace(1) @__mlisp_alloc_pair_gc_as1"));
    assert!(compiled.llvm_ir.contains("call i64 @__lambda_0(i64 3, i64 5, i64"));
}

#[test]
fn emits_symbol_form_rest_lambda() {
    let ast = parse_program("((lambda args args) 1 2)\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("rest_only_lambda_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("define i64 @__lambda_0(i64 %0)"));
    assert!(compiled.llvm_ir.contains("call i64 @__lambda_0(i64"));
}

#[test]
fn compiles_apply_for_fixed_and_variadic_procedures() {
    let fixed_ast = parse_program("(define (add3 a b c) (+ a (+ b c))) (apply add3 '(1 2 3))\n").unwrap();
    let variadic_ast = parse_program("(define (tail x . rest) rest) (apply tail 1 '(2 3))\n").unwrap();

    let fixed_hir = lower_program(&fixed_ast).unwrap();
    let variadic_hir = lower_program(&variadic_ast).unwrap();

    let fixed_ir = LlvmBackend::compile_program("apply_fixed_module", &fixed_hir).unwrap();
    let variadic_ir = LlvmBackend::compile_program("apply_variadic_module", &variadic_hir).unwrap();

    assert!(fixed_ir.llvm_ir.contains("@mlisp_list_length"));
    assert!(fixed_ir.llvm_ir.contains("@mlisp_list_ref"));
    assert!(fixed_ir.llvm_ir.contains("call i64 @add3("));

    assert!(variadic_ir.llvm_ir.contains("@mlisp_list_tail"));
    assert!(variadic_ir.llvm_ir.contains("call i64 @tail("));
}

#[test]
fn compiles_apply_for_closure_values() {
    let ast =
        parse_program("(let ((f (lambda (a b c) (+ a (+ b c))))) (apply f '(1 2 3)))\n").unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("apply_closure_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("@mlisp_list_length"));
    assert!(compiled.llvm_ir.contains("@mlisp_list_ref"));
    assert!(compiled.llvm_ir.contains("call i64 @__lambda_0("));
}

#[test]
fn compiles_values_and_call_with_values() {
    let values_ast = parse_program("(values 1 2 3)\n").unwrap();
    let call_ast = parse_program(
        "(call-with-values (lambda () (values 1 2 3)) (lambda (a b . rest) (list a b rest)))\n",
    )
    .unwrap();

    let values_hir = lower_program(&values_ast).unwrap();
    let call_hir = lower_program(&call_ast).unwrap();

    let values_ir = LlvmBackend::compile_program("values_module", &values_hir).unwrap();
    let call_ir = LlvmBackend::compile_program("call_with_values_module", &call_hir).unwrap();

    assert!(values_ir.llvm_ir.contains("@mlisp_alloc_values"));
    assert!(call_ir.llvm_ir.contains("@mlisp_is_values"));
    assert!(call_ir.llvm_ir.contains("@mlisp_values_length"));
    assert!(call_ir.llvm_ir.contains("@mlisp_values_ref"));
    assert!(call_ir.llvm_ir.contains("@mlisp_values_tail_list"));
}

#[test]
fn compiles_raise_error_and_guard() {
    let raise_ast = parse_program("(guard (exn (else exn)) (raise 7))\n").unwrap();
    let error_ast = parse_program("(guard (exn (else exn)) (error \"boom\" 1 2))\n").unwrap();

    let raise_hir = lower_program(&raise_ast).unwrap();
    let error_hir = lower_program(&error_ast).unwrap();

    let raise_ir = LlvmBackend::compile_program("raise_guard_module", &raise_hir).unwrap();
    let error_ir = LlvmBackend::compile_program("error_guard_module", &error_hir).unwrap();

    assert!(raise_ir.llvm_ir.contains("@rt_raise"));
    assert!(raise_ir.llvm_ir.contains("@rt_exception_pending"));
    assert!(raise_ir.llvm_ir.contains("@rt_take_pending_exception"));
    assert!(error_ir.llvm_ir.contains("@rt_raise"));
    assert!(error_ir.llvm_ir.contains("guard.handler"));
}

#[test]
fn compiles_first_class_builtin_procedures() {
    let call_ast = parse_program("(let ((f +)) (f 1 2 3))\n").unwrap();
    let apply_ast = parse_program("(let ((f +)) (apply f '(1 2 3)))\n").unwrap();

    let call_hir = lower_program(&call_ast).unwrap();
    let apply_hir = lower_program(&apply_ast).unwrap();

    let call_ir = LlvmBackend::compile_program("builtin_call_module", &call_hir).unwrap();
    let apply_ir = LlvmBackend::compile_program("builtin_apply_module", &apply_hir).unwrap();

    assert!(call_ir.llvm_ir.contains("define i64 @__builtin__"));
    assert!(call_ir.llvm_ir.contains("call i64 @__builtin__("));
    assert!(apply_ir.llvm_ir.contains("call i64 @__builtin__("));
    assert!(apply_ir.llvm_ir.contains("@mlisp_list_length"));
    assert!(apply_ir.llvm_ir.contains("@mlisp_list_ref"));
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
fn emits_tail_call_for_direct_self_tail_recursion() {
    let source = "\
        (define (countdown n)
          (if (zero? n)
              0
              (countdown (- n 1))))
        (countdown 3)
    ";
    let ast = parse_program(source).unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("musttail_direct_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("tail call i64 @countdown(i64"));
}

#[test]
fn emits_tail_call_for_recursive_closure_tail_calls() {
    let source = "\
        (letrec ((countdown
                    (lambda (n)
                      (if (zero? n)
                          0
                          (countdown (- n 1))))))
          (countdown 3))
    ";
    let ast = parse_program(source).unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("musttail_closure_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("tail call i64 %closure.tail.fn"));
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

    assert!(compiled.llvm_ir.contains("@__mlisp_alloc_pair_gc_as1"));
    assert!(compiled.llvm_ir.contains("call void @rt_root_slot_push"));
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

    assert!(pair_ir.llvm_ir.contains("@__mlisp_alloc_pair_gc_as1"));
    assert!(pair_ir.llvm_ir.contains("call void @rt_root_slot_push"));
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
fn compiles_symbol_string_and_character_procedures() {
    let ast = parse_program(
        "(begin (symbol->string 'hello) (string->symbol \"world\") (char=? #\\a #\\a) (char<? #\\a #\\b) (char->integer #\\A) (integer->char 66))\n",
    )
    .unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("symbol_char_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("@mlisp_symbol_to_string"));
    assert!(compiled.llvm_ir.contains("@mlisp_string_to_symbol"));
    assert!(compiled.llvm_ir.contains("char_cmp"));
    assert!(compiled.llvm_ir.contains("integer_to_char"));
}

#[test]
fn compiles_equal_for_structural_values() {
    let pair_ast = parse_program("(equal? '(1 2) '(1 2))\n").unwrap();
    let vector_ast = parse_program("(equal? (vector 1 2) (vector 1 2))\n").unwrap();
    let string_ast = parse_program("(equal? \"hi\" \"hi\")\n").unwrap();

    let pair_hir = lower_program(&pair_ast).unwrap();
    let vector_hir = lower_program(&vector_ast).unwrap();
    let string_hir = lower_program(&string_ast).unwrap();

    let pair_ir = LlvmBackend::compile_program("equal_pair_module", &pair_hir).unwrap();
    let vector_ir = LlvmBackend::compile_program("equal_vector_module", &vector_hir).unwrap();
    let string_ir = LlvmBackend::compile_program("equal_string_module", &string_hir).unwrap();

    assert!(pair_ir.llvm_ir.contains("@mlisp_equal"));
    assert!(vector_ir.llvm_ir.contains("@mlisp_equal"));
    assert!(string_ir.llvm_ir.contains("@mlisp_equal"));
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
fn compiles_map_foreach_and_member_assoc() {
    let ast = parse_program(
        "(begin (map + '(1 2) '(3 4)) (for-each (lambda (x) x) '(1 2)) (member '(1) '((0) (1) (2))) (assoc 'b '((a . 1) (b . 2))))\n",
    )
    .unwrap();
    let hir = lower_program(&ast).unwrap();
    let compiled = LlvmBackend::compile_program("list_library_module", &hir).unwrap();

    assert!(compiled.llvm_ir.contains("list_iter.cond"));
    assert!(compiled.llvm_ir.contains("@mlisp_member"));
    assert!(compiled.llvm_ir.contains("@mlisp_assoc"));
    assert!(compiled.llvm_ir.contains("@__builtin__"));
}

#[test]
fn compiles_pair_mutation_and_list_copy_reverse() {
    let mutate_ast = parse_program("(let ((p (cons 1 2))) (begin (set-car! p 9) (set-cdr! p '()) p))\n").unwrap();
    let copy_ast = parse_program("(list-copy (list 1 2 3))\n").unwrap();
    let reverse_ast = parse_program("(reverse (list 1 2 3))\n").unwrap();
    let mutate_hir = lower_program(&mutate_ast).unwrap();
    let copy_hir = lower_program(&copy_ast).unwrap();
    let reverse_hir = lower_program(&reverse_ast).unwrap();
    let mutate_ir = LlvmBackend::compile_program("pair_set_module", &mutate_hir).unwrap();
    let copy_ir = LlvmBackend::compile_program("list_copy_module", &copy_hir).unwrap();
    let reverse_ir = LlvmBackend::compile_program("reverse_module", &reverse_hir).unwrap();

    assert!(mutate_ir.llvm_ir.contains("@mlisp_pair_set_car"));
    assert!(mutate_ir.llvm_ir.contains("@mlisp_pair_set_cdr"));
    assert!(copy_ir.llvm_ir.contains("@mlisp_list_copy"));
    assert!(reverse_ir.llvm_ir.contains("@mlisp_reverse"));
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
