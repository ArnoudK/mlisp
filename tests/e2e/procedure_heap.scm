(define (mkpair) (cons 7 8))
(define (head p) (car p))
(define (second v) (vector-ref v 1))

(let ((p (mkpair))
      (v (vector 7 8)))
  (begin
    (gc-stress 32)
    (display (head p))
    (newline)
    (display (second v))
    (newline)
    (display v)
    0))
