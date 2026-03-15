(define-syntax pair-sums
  (syntax-rules ()
    ((pair-sums ((a b) ...))
     (list (+ a b) ...))))

(define-syntax gather
  (syntax-rules ()
    ((gather (a ...) (b ...))
     (list a ... b ...))))

(write (pair-sums ((1 2) (3 4))))
(newline)
(write (gather (1 2) (3 4)))
(newline)
0
