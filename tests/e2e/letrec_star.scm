(begin
  (display
    (letrec* ((f (lambda () 1))
              (g (lambda () (f))))
      (g)))
  (newline)
  0)
