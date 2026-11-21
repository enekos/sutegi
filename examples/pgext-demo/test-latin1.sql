-- Text needs a UTF8 database: in LATIN1 the runtime refuses instead of
-- handing Rust bytes that aren't UTF-8.
CREATE EXTENSION pgext_demo;
DO $$
BEGIN
  ASSERT safe_div(9, 3) = 3, 'non-text functions still work';
  PERFORM hello('x');
  RAISE 'hello did not fail in LATIN1';
EXCEPTION WHEN feature_not_supported THEN NULL;
END $$;
