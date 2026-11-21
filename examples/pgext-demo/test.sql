-- Asserts by raising; run by scripts/pgext-test.sh with ON_ERROR_STOP.
CREATE EXTENSION pgext_demo;
CREATE TABLE t (v int);
INSERT INTO t SELECT generate_series(1, 5);

DO $$
DECLARE
  det text;
  hnt text;
  msg text;
BEGIN
  -- scalars, NULLs, strictness
  ASSERT hello('world') = 'Hello, world!', 'hello';
  ASSERT hello('ñandú') = 'Hello, ñandú!', 'hello utf8';
  ASSERT hello(NULL) IS NULL, 'strict skips NULL';
  ASSERT length(hello(repeat('x', 1000000))) = 1000008, 'large text';
  ASSERT first_of(NULL, 2) = 2 AND first_of(1, NULL) = 1, 'first_of';
  ASSERT first_of(NULL, NULL) IS NULL, 'first_of null';
  ASSERT mean(1.0, 2.0::real) = 1.5, 'mean';
  ASSERT flip(5::smallint, true) = -5 AND flip(5::smallint, false) = 5, 'flip';
  ASSERT reverse_bytes('\x010203'::bytea) = '\x030201'::bytea, 'bytea';
  ASSERT reverse_bytes(''::bytea) = ''::bytea, 'empty bytea';
  ASSERT parse_int(' 42 ') = 42, 'parse_int';

  -- sets
  ASSERT (SELECT sum(n) FROM count_to(4) AS n) = 10, 'count_to';
  ASSERT (SELECT count(*) FROM count_to(0)) = 0, 'empty set';
  ASSERT (SELECT count(*) FROM count_to(100000)) = 100000, 'big set';
  ASSERT (SELECT array_agg(p) FROM split_nonempty('a,,b', ',') AS p)::text = '{a,NULL,b}', 'set NULLs';

  -- SPI, logging, nested calls
  ASSERT count_above('t', 2) = 3, 'count_above';
  ASSERT log_message('a') = 1 AND log_message('b') = 2, 'log_message';
  ASSERT hello_via_spi('nested') = 'Hello, nested!', 'nested';

  -- errors carry SQLSTATE, detail and hint
  BEGIN
    PERFORM safe_div(1, 0);
    RAISE 'safe_div did not fail';
  EXCEPTION WHEN division_by_zero THEN
    GET STACKED DIAGNOSTICS det = PG_EXCEPTION_DETAIL, hnt = PG_EXCEPTION_HINT;
    ASSERT det = '1 / 0', 'detail: ' || det;
    ASSERT hnt = 'Pass a non-zero divisor.', 'hint: ' || hnt;
  END;
  BEGIN
    PERFORM parse_int('x');
    RAISE 'parse_int did not fail';
  EXCEPTION WHEN raise_exception THEN
    GET STACKED DIAGNOSTICS msg = MESSAGE_TEXT;
    ASSERT msg LIKE 'invalid digit%', 'parse error: ' || msg;
  END;

  -- PostgreSQL errors raised under Rust come back unchanged
  BEGIN
    PERFORM bad_query();
    RAISE 'bad_query did not fail';
  EXCEPTION WHEN undefined_table THEN NULL;
  END;
  BEGIN
    PERFORM swallow_error();
    RAISE 'swallow_error did not fail';
  EXCEPTION WHEN division_by_zero THEN NULL;
  END;

  -- a panic is an internal error; the backend survives
  BEGIN
    PERFORM boom();
    RAISE 'boom did not fail';
  EXCEPTION WHEN internal_error THEN
    GET STACKED DIAGNOSTICS msg = MESSAGE_TEXT;
    ASSERT msg LIKE 'Rust panic in boom: kaboom%', 'panic: ' || msg;
  END;
  ASSERT hello('again') = 'Hello, again!', 'alive after panic';
END $$;

-- cancellation reaches Rust through check_for_interrupts
SET statement_timeout = '200ms';
DO $$
BEGIN
  PERFORM spin();
  RAISE 'spin was not cancelled';
EXCEPTION WHEN query_canceled THEN NULL;
END $$;
RESET statement_timeout;

-- a SQL declaration that disagrees with the Rust signature is refused
CREATE FUNCTION wrong_type(int) RETURNS text
  AS '$libdir/pgext_demo', 'sx_fn_hello' LANGUAGE C STRICT;
CREATE FUNCTION wrong_arity() RETURNS text
  AS '$libdir/pgext_demo', 'sx_fn_hello' LANGUAGE C;
DO $$
BEGIN
  BEGIN
    PERFORM wrong_type(1);
    RAISE 'wrong_type did not fail';
  EXCEPTION WHEN datatype_mismatch THEN NULL;
  END;
  BEGIN
    PERFORM wrong_arity();
    RAISE 'wrong_arity did not fail';
  EXCEPTION WHEN internal_error THEN NULL;
  END;
END $$;

-- hardening (sutegi-pgext battle test)
SET statement_timeout = '200ms';
DO $$
BEGIN
  PERFORM count(*) FROM forever();
  RAISE 'forever was not cancelled';
EXCEPTION WHEN query_canceled THEN NULL;
END $$;
RESET statement_timeout;

DO $$
DECLARE
  before bigint;
  growth bigint;
BEGIN
  -- lazy sets: a million rows without holding them in Rust
  ASSERT (SELECT count(*) FROM count_to(1000000)) = 1000000, 'lazy set';
  ASSERT (SELECT array_agg(x) FROM (SELECT count_to(3) AS x) s)::text = '{1,2,3}', 'set in target list';

  -- text can't carry NUL
  BEGIN
    PERFORM nul_text();
    RAISE 'nul_text did not fail';
  EXCEPTION WHEN character_not_in_repertoire THEN NULL;
  END;

  -- SPI results are freed per query: 2000 x 100 kB must not pile up
  growth := spi_growth(2000, 100000);
  ASSERT growth < 10000000, 'SPI memory grew by ' || growth;

  -- query_scalar runs the statement to completion
  before := (SELECT count(*) FROM demo_log);
  PERFORM insert_many(5);
  ASSERT (SELECT count(*) FROM demo_log) = before + 5, 'insert_many wrote every row';

  -- deep Rust recursion stops at max_stack_depth instead of SIGSEGV
  ASSERT deep(100) = 100, 'shallow recursion';
  BEGIN
    PERFORM deep(100000000);
    RAISE 'deep did not fail';
  EXCEPTION WHEN statement_too_complex THEN NULL;
  END;

  -- SECURITY DEFINER pins search_path
  ASSERT definer_search_path() = 'pg_catalog, pg_temp', 'definer search_path';

  -- a bad size is an ordinary ERROR
  BEGIN
    PERFORM alloc_bytes(-1);
    RAISE 'alloc_bytes(-1) did not fail';
  EXCEPTION WHEN raise_exception THEN NULL;
  END;
  ASSERT alloc_bytes(1000) = 1000, 'small allocation';
END $$;
