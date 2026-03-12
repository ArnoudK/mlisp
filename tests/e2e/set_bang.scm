(begin
  (display (let ((x 1))
             (begin
               (set! x 2)
               x)))
  (newline)
  (display
    (let ((x 1))
      (let ((f (lambda () x)))
        (begin
          (set! x 2)
          (f)))))
  (newline)
  0)
