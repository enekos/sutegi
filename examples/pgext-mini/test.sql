-- Two sutegi-pgext extensions in one backend, with clashing Rust symbols.
CREATE EXTENSION pgext_demo;
CREATE EXTENSION pgext_mini;
DO $$
BEGIN
  ASSERT hello('x') = 'Hello, x!', 'demo hello';
  ASSERT mini_hello('x') = 'mini x', 'mini hello';
  ASSERT hello('y') || mini_hello('y') = 'Hello, y!mini y', 'interleaved';
END $$;
