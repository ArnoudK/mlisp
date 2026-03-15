(define (countdown n acc)
  (if (zero? n)
      acc
      (countdown (- n 1) (+ acc 1))))

(display (countdown 50 0))
(newline)

(letrec ((even?
           (lambda (n)
             (if (zero? n)
                 #t
                 (odd? (- n 1)))))
         (odd?
           (lambda (n)
             (if (zero? n)
                 #f
                 (even? (- n 1))))))
  (write (even? 51))
  (newline))

0
