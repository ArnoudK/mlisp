(begin
  (display (eq? #\x #\x))
  (newline)
  (let ((p (cons 1 2)))
    (display (eqv? p p))
    (newline)
    (display (eq? p (cons 1 2)))
    (newline))
  0)
