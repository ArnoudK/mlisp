(let ((x 9))
  (letrec ((countdown
              (lambda (n)
                (if (zero? n) x (countdown (- n 1))))))
    (begin
      (display (countdown 3))
      (newline)
      0)))
