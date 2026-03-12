(begin
  (display (and 1 2))
  (newline)
  (display (or #f 3))
  (newline)
  (when #t
    (display 4)
    (newline))
  (unless #f
    (display 5)
    (newline))
  (display (cond ((zero? 1) 6) ((zero? 0) 7) (else 8)))
  (newline)
  0)
