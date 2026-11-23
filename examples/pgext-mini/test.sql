-- Two sutegi-pgext extensions in one backend, with clashing Rust symbols.
CREATE EXTENSION pgext_demo;
CREATE EXTENSION pgext_mini;
DO $$
BEGIN
  ASSERT hello('x') = 'Hello, x!', 'demo hello';
  ASSERT mini_hello('x') = 'mini x', 'mini hello';
  ASSERT hello('y') || mini_hello('y') = 'Hello, y!mini y', 'interleaved';
  ASSERT ARRAY(SELECT n FROM mini_series(arg3 => 99, arg2 => 3) n ORDER BY n) = ARRAY[1,2,3];
  ASSERT (SELECT prorows = 1000 AND procost = 16 FROM pg_proc
          WHERE oid = 'mini_series(integer,integer)'::regprocedure);
END $$;
