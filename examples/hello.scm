(define (add a b)
  (+ a b))

(let* ((answer 40)
       (bump (lambda (x) (add x 2))))
  (if #t (bump answer) 0))
