(define-library (lib demo)
  (export add2 with-add2)
  (begin
    (define (add2 x) (+ x 2))
    (define-syntax with-add2
      (syntax-rules ()
        ((with-add2 value)
         (add2 value))))))
