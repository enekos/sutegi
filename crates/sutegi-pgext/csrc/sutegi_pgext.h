/*
 * The ABI between the C runtime and the sutegi-pgext Rust crate.
 *
 * Rust never sees a PostgreSQL struct: it holds an opaque `sx_call *` and
 * moves values through `sx_value`. Every function here that can raise a
 * PostgreSQL error catches it (PG_TRY), parks it on the call and returns
 * false, so an ereport/longjmp never unwinds a Rust frame. The parked error
 * is re-thrown by sx_invoke after Rust has returned.
 *
 * Keep in sync with crates/sutegi-pgext/src/ffi.rs.
 */
#ifndef SUTEGI_PGEXT_H
#define SUTEGI_PGEXT_H

#include "postgres.h"
#include "fmgr.h"

typedef enum sx_kind
{
	SX_VOID = 0,
	SX_BOOL = 1,
	SX_INT2 = 2,
	SX_INT4 = 3,
	SX_INT8 = 4,
	SX_FLOAT4 = 5,
	SX_FLOAT8 = 6,
	SX_TEXT = 7,
	SX_BYTEA = 8
} sx_kind;

/* One SQL value. Integers and bools use i, floats f, text and bytea ptr/len. */
typedef struct sx_value
{
	int32		kind;
	int32		isnull;
	int64		i;
	double		f;
	const char *ptr;
	size_t		len;
} sx_value;

typedef struct sx_call sx_call;

typedef void (*sx_rustfn) (sx_call *call);

/*
 * A copied SPI result. It lives in its own memory context (cxt), which
 * sx_spi_free deletes once Rust has copied it, so a loop of queries doesn't
 * grow the calling function's memory.
 */
typedef struct sx_spi_result
{
	uint64		nrows;
	int32		ncols;
	uint64		processed;
	char	  **names;
	sx_value   *values;			/* nrows * ncols, row-major, kind SX_TEXT */
	void	   *cxt;
} sx_spi_result;

/* Rust's out-of-memory hook: never returns. */
typedef void (*sx_oom_fn) (size_t size);

extern Datum sx_invoke(FunctionCallInfo fcinfo, sx_rustfn fn);

extern int32 sx_nargs(const sx_call *call);
extern bool sx_arg(sx_call *call, int32 index, int32 kind, sx_value *out);
extern bool sx_ret(sx_call *call, const sx_value *value);
extern bool sx_srf_begin(sx_call *call, int32 kind);
extern bool sx_srf_push(sx_call *call, const sx_value *value);
extern void sx_set_error(sx_call *call, const char *sqlstate,
						 const char *message, size_t message_len,
						 const char *detail, size_t detail_len,
						 const char *hint, size_t hint_len);
extern bool sx_log(sx_call *call, int32 level, const char *message, size_t len);
extern bool sx_check_interrupts(sx_call *call);
extern bool sx_spi_exec(sx_call *call, const char *sql, size_t sql_len,
						int32 nparams, const sx_value *params, int64 limit,
						sx_spi_result **out);
extern void sx_spi_free(sx_spi_result *res);
extern bool sx_stack_too_deep(void);

/* Defined by the Rust crate; _PG_init hands it the out-of-memory hook. */
extern void sutegi_pgext_on_load(sx_oom_fn oom);
extern void _PG_init(void);

#endif
