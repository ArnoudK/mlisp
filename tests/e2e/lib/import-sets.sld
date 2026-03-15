(define-library (lib import-sets)
  (export (rename add2 sum2) inc)
  (begin
    (define (add2 x) (+ x 2))
    (define-syntax inc
      (syntax-rules ()
        ((inc x) (add2 x))))))
