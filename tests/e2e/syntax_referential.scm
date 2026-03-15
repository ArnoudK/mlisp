(define helper 1)

(define-syntax use-helper
  (syntax-rules ()
    ((use-helper) helper)))

(write
  (let ((helper 9))
    (use-helper)))
(newline)
0
