(define-syntax capture-test
  (syntax-rules ()
    ((capture-test x)
     (let ((tmp 1))
       x))))

(write
  (let ((tmp 9))
    (capture-test tmp)))
(newline)
0
