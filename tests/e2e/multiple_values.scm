(begin
  (write (call-with-values (lambda () (values)) (lambda () 'empty)))
  (newline)
  (write (call-with-values (lambda () (values 1)) (lambda (x) x)))
  (newline)
  (write
    (call-with-values
      (lambda () (values 1 2 3))
      (lambda (a b . rest) (list a b rest))))
  (newline)
  0)
