(begin
  (guard (exn
           ((pair? exn) (begin (write 'caught-error) (newline) 0))
           (else (begin (write 'bad-error) (newline) 1)))
    (begin
      (error "boom" 1 2)
      2))
  (guard (exn
           ((equal? exn '(7)) (begin (write 'caught) (newline) 0))
           (else (begin (write 'bad) (newline) 1)))
    (begin
      (raise '(7))
      2))
  0)
