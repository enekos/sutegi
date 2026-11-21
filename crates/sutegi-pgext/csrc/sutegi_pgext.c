/*
 * The sutegi-pgext C runtime: compiled by PGXS into every extension next to
 * the generated functions.c and the extension's Rust static library.
 *
 * The one rule: a PostgreSQL error (ereport -> siglongjmp) must never unwind
 * a Rust frame. Every entry point Rust calls wraps its PostgreSQL work in
 * SX_GUARD, which catches the error, parks a copy on the call and returns
 * false; Rust then unwinds normally with an error, and sx_invoke re-throws the
 * parked error once Rust has returned. A parked error also poisons the call:
 * every later helper fails at once, so Rust can't keep driving PostgreSQL
 * after an error it was supposed to propagate.
 */
#include "postgres.h"

#include "access/htup_details.h"
#include "catalog/pg_proc.h"
#include "catalog/pg_type.h"
#include "executor/spi.h"
#include "fmgr.h"
#include "funcapi.h"
#include "mb/pg_wchar.h"
#include "miscadmin.h"
#include "nodes/execnodes.h"
#include "utils/builtins.h"
#include "utils/lsyscache.h"
#include "utils/memutils.h"
#include "utils/tuplestore.h"

#include "sutegi_pgext.h"

StaticAssertDecl(sizeof(sx_value) == 40, "sx_value layout");
StaticAssertDecl(offsetof(sx_value, i) == 8, "sx_value layout");
StaticAssertDecl(offsetof(sx_value, ptr) == 24, "sx_value layout");
StaticAssertDecl(sizeof(sx_spi_result) == 40, "sx_spi_result layout");

struct sx_call
{
	FunctionCallInfo fcinfo;
	ErrorData  *pending;
	bool		has_error;
	char		sqlstate[6];
	char	   *message;
	char	   *detail;
	char	   *hint;
	bool		returned;
	bool		isnull;
	Datum		result;
	bool		srf;
	Oid			srf_type;
	Tuplestorestate *tupstore;
	TupleDesc	tupdesc;
};

static void
sx_park(sx_call *call, MemoryContext cxt)
{
	ErrorData  *edata;

	MemoryContextSwitchTo(cxt);
	edata = CopyErrorData();
	FlushErrorState();
	if (call->pending == NULL)
		call->pending = edata;
	else
		FreeErrorData(edata);
}

#define SX_GUARD(call, ok, ...) \
	do { \
		MemoryContext sx_cxt_ = CurrentMemoryContext; \
		PG_TRY(); \
		{ \
			__VA_ARGS__; \
		} \
		PG_CATCH(); \
		{ \
			sx_park((call), sx_cxt_); \
			(ok) = false; \
		} \
		PG_END_TRY(); \
	} while (0)

static bool
sx_poisoned(const sx_call *call)
{
	return call->pending != NULL || call->has_error;
}

static Oid
sx_kind_oid(int32 kind)
{
	switch (kind)
	{
		case SX_BOOL:
			return BOOLOID;
		case SX_INT2:
			return INT2OID;
		case SX_INT4:
			return INT4OID;
		case SX_INT8:
			return INT8OID;
		case SX_FLOAT4:
			return FLOAT4OID;
		case SX_FLOAT8:
			return FLOAT8OID;
		case SX_TEXT:
			return TEXTOID;
		case SX_BYTEA:
			return BYTEAOID;
		case SX_VOID:
			return VOIDOID;
		default:
			return InvalidOid;
	}
}

static void
sx_require_utf8(void)
{
	int			enc = GetDatabaseEncoding();

	if (enc != PG_UTF8 && enc != PG_SQL_ASCII)
		ereport(ERROR,
				(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
				 errmsg("sutegi-pgext text values need a UTF8 database, not %s",
						GetDatabaseEncodingName())));
}

static void
sx_check_type(Oid actual, int32 kind, const char *what)
{
	Oid			expected = sx_kind_oid(kind);

	if (expected == InvalidOid)
		ereport(ERROR,
				(errcode(ERRCODE_INTERNAL_ERROR),
				 errmsg_internal("sutegi-pgext: unknown value kind %d", kind)));
	if (OidIsValid(actual) && actual != expected)
		ereport(ERROR,
				(errcode(ERRCODE_DATATYPE_MISMATCH),
				 errmsg("sutegi-pgext: %s has type %s, the Rust function expects %s",
						what, format_type_be(actual), format_type_be(expected)),
				 errhint("The installed SQL script is out of date: rebuild and reinstall the extension.")));
}

/* Builds a Datum in CurrentMemoryContext. Raises on bad input or OOM. */
static Datum
sx_to_datum(const sx_value *v)
{
	switch (v->kind)
	{
		case SX_VOID:
			return (Datum) 0;
		case SX_BOOL:
			return BoolGetDatum(v->i != 0);
		case SX_INT2:
			return Int16GetDatum((int16) v->i);
		case SX_INT4:
			return Int32GetDatum((int32) v->i);
		case SX_INT8:
			return Int64GetDatum(v->i);
		case SX_FLOAT4:
			return Float4GetDatum((float4) v->f);
		case SX_FLOAT8:
			return Float8GetDatum(v->f);
		case SX_TEXT:
		case SX_BYTEA:
			{
				bytea	   *out;

				if (v->kind == SX_TEXT)
					sx_require_utf8();
				if (v->len > MaxAllocSize - VARHDRSZ)
					ereport(ERROR,
							(errcode(ERRCODE_PROGRAM_LIMIT_EXCEEDED),
							 errmsg("sutegi-pgext: value of %zu bytes is too large", v->len)));
				out = (bytea *) palloc(v->len + VARHDRSZ);
				SET_VARSIZE(out, v->len + VARHDRSZ);
				if (v->len > 0)
					memcpy(VARDATA(out), v->ptr, v->len);
				return PointerGetDatum(out);
			}
		default:
			ereport(ERROR,
					(errcode(ERRCODE_INTERNAL_ERROR),
					 errmsg_internal("sutegi-pgext: unknown value kind %d", v->kind)));
	}
	pg_unreachable();
}

Datum
sx_invoke(FunctionCallInfo fcinfo, sx_rustfn fn)
{
	sx_call		call;

	check_stack_depth();
	memset(&call, 0, sizeof(call));
	call.fcinfo = fcinfo;

	PG_TRY();
	{
		fn(&call);
	}
	PG_CATCH();
	{
		/* Unreachable while every helper guards its PostgreSQL calls. */
		ereport(WARNING,
				(errmsg_internal("sutegi-pgext: a PostgreSQL error unwound Rust frames")));
		PG_RE_THROW();
	}
	PG_END_TRY();

	if (call.pending != NULL)
		ReThrowError(call.pending);
	if (call.has_error)
		ereport(ERROR,
				(errcode(MAKE_SQLSTATE(call.sqlstate[0], call.sqlstate[1],
									   call.sqlstate[2], call.sqlstate[3],
									   call.sqlstate[4])),
				 errmsg_internal("%s", call.message),
				 call.detail ? errdetail_internal("%s", call.detail) : 0,
				 call.hint ? errhint("%s", call.hint) : 0));
	if (call.srf)
	{
		fcinfo->isnull = true;
		return (Datum) 0;
	}
	if (!call.returned)
		ereport(ERROR,
				(errcode(ERRCODE_INTERNAL_ERROR),
				 errmsg_internal("sutegi-pgext: the Rust function returned no value")));
	fcinfo->isnull = call.isnull;
	return call.result;
}

int32
sx_nargs(const sx_call *call)
{
	return call->fcinfo->nargs;
}

bool
sx_arg(sx_call *call, int32 index, int32 kind, sx_value *out)
{
	volatile bool ok = true;
	FunctionCallInfo fcinfo = call->fcinfo;

	memset(out, 0, sizeof(*out));
	out->kind = kind;
	if (sx_poisoned(call))
		return false;

	SX_GUARD(call, ok, {
		if (index < 0 || index >= fcinfo->nargs)
			ereport(ERROR,
					(errcode(ERRCODE_INTERNAL_ERROR),
					 errmsg_internal("sutegi-pgext: argument %d of %d", index, fcinfo->nargs)));
		sx_check_type(get_fn_expr_argtype(fcinfo->flinfo, index), kind, "an argument");
		if (PG_ARGISNULL(index))
			out->isnull = 1;
		else
		{
			Datum		d = PG_GETARG_DATUM(index);

			switch (kind)
			{
				case SX_BOOL:
					out->i = DatumGetBool(d) ? 1 : 0;
					break;
				case SX_INT2:
					out->i = DatumGetInt16(d);
					break;
				case SX_INT4:
					out->i = DatumGetInt32(d);
					break;
				case SX_INT8:
					out->i = DatumGetInt64(d);
					break;
				case SX_FLOAT4:
					out->f = DatumGetFloat4(d);
					break;
				case SX_FLOAT8:
					out->f = DatumGetFloat8(d);
					break;
				case SX_TEXT:
				case SX_BYTEA:
					{
						struct varlena *v;

						if (kind == SX_TEXT)
							sx_require_utf8();
						v = PG_DETOAST_DATUM_PACKED(d);
						out->ptr = VARDATA_ANY(v);
						out->len = VARSIZE_ANY_EXHDR(v);
						break;
					}
				default:
					ereport(ERROR,
							(errcode(ERRCODE_INTERNAL_ERROR),
							 errmsg_internal("sutegi-pgext: kind %d can't be an argument", kind)));
			}
		}
	});
	return ok;
}

bool
sx_ret(sx_call *call, const sx_value *value)
{
	volatile bool ok = true;
	FunctionCallInfo fcinfo = call->fcinfo;

	if (sx_poisoned(call))
		return false;

	SX_GUARD(call, ok, {
		if (call->returned || call->srf)
			ereport(ERROR,
					(errcode(ERRCODE_INTERNAL_ERROR),
					 errmsg_internal("sutegi-pgext: the function returned twice")));
		sx_check_type(get_fn_expr_rettype(fcinfo->flinfo), value->kind, "the result");
		if (value->isnull)
		{
			call->isnull = true;
			call->result = (Datum) 0;
		}
		else
			call->result = sx_to_datum(value);
		call->returned = true;
	});
	return ok;
}

bool
sx_srf_begin(sx_call *call, int32 kind)
{
	volatile bool ok = true;
	FunctionCallInfo fcinfo = call->fcinfo;

	if (sx_poisoned(call))
		return false;

	SX_GUARD(call, ok, {
		ReturnSetInfo *rsinfo = (ReturnSetInfo *) fcinfo->resultinfo;
		MemoryContext old;

		if (call->returned || call->srf)
			ereport(ERROR,
					(errcode(ERRCODE_INTERNAL_ERROR),
					 errmsg_internal("sutegi-pgext: the function returned twice")));
		if (rsinfo == NULL || !IsA(rsinfo, ReturnSetInfo) ||
			(rsinfo->allowedModes & SFRM_Materialize) == 0)
			ereport(ERROR,
					(errcode(ERRCODE_FEATURE_NOT_SUPPORTED),
					 errmsg("set-valued function called in context that cannot accept a set")));
		sx_check_type(get_fn_expr_rettype(fcinfo->flinfo), kind, "the result");

		old = MemoryContextSwitchTo(rsinfo->econtext->ecxt_per_query_memory);
		call->tupdesc = CreateTemplateTupleDesc(1);
		TupleDescInitEntry(call->tupdesc, (AttrNumber) 1, "value",
						   sx_kind_oid(kind), -1, 0);
		call->tupstore = tuplestore_begin_heap((rsinfo->allowedModes & SFRM_Materialize_Random) != 0,
											   false, work_mem);
		MemoryContextSwitchTo(old);

		rsinfo->returnMode = SFRM_Materialize;
		rsinfo->setResult = call->tupstore;
		rsinfo->setDesc = call->tupdesc;
		call->srf = true;
		call->srf_type = kind;
	});
	return ok;
}

bool
sx_srf_push(sx_call *call, const sx_value *value)
{
	volatile bool ok = true;

	if (sx_poisoned(call))
		return false;

	SX_GUARD(call, ok, {
		Datum		d = (Datum) 0;
		bool		isnull = value->isnull != 0;

		if (!call->srf || value->kind != call->srf_type)
			ereport(ERROR,
					(errcode(ERRCODE_INTERNAL_ERROR),
					 errmsg_internal("sutegi-pgext: set row pushed outside a set")));
		if (!isnull)
			d = sx_to_datum(value);
		tuplestore_putvalues(call->tupstore, call->tupdesc, &d, &isnull);
	});
	return ok;
}

static bool
sx_valid_sqlstate(const char *s)
{
	for (int i = 0; i < 5; i++)
		if (!((s[i] >= '0' && s[i] <= '9') || (s[i] >= 'A' && s[i] <= 'Z')))
			return false;
	return true;
}

void
sx_set_error(sx_call *call, const char *sqlstate,
			 const char *message, size_t message_len,
			 const char *detail, size_t detail_len,
			 const char *hint, size_t hint_len)
{
	volatile bool ok = true;

	if (sx_poisoned(call))
		return;

	SX_GUARD(call, ok, {
		memcpy(call->sqlstate, sx_valid_sqlstate(sqlstate) ? sqlstate : "XX000", 5);
		call->sqlstate[5] = '\0';
		call->message = pnstrdup(message, message_len);
		call->detail = detail ? pnstrdup(detail, detail_len) : NULL;
		call->hint = hint ? pnstrdup(hint, hint_len) : NULL;
		call->has_error = true;
	});
	(void) ok;
}

bool
sx_log(sx_call *call, int32 level, const char *message, size_t len)
{
	volatile bool ok = true;
	int			elevel;
	int			n = len > INT_MAX ? INT_MAX : (int) len;

	if (sx_poisoned(call))
		return false;

	switch (level)
	{
		case 0:
			elevel = DEBUG1;
			break;
		case 1:
			elevel = LOG;
			break;
		case 2:
			elevel = INFO;
			break;
		case 3:
			elevel = NOTICE;
			break;
		default:
			elevel = WARNING;
			break;
	}
	SX_GUARD(call, ok, {
		ereport(elevel, (errmsg_internal("%.*s", n, message)));
	});
	return ok;
}

bool
sx_check_interrupts(sx_call *call)
{
	volatile bool ok = true;

	if (sx_poisoned(call))
		return false;

	SX_GUARD(call, ok, {
		CHECK_FOR_INTERRUPTS();
	});
	return ok;
}

static bool
sx_read_only(FunctionCallInfo fcinfo)
{
	return fcinfo->flinfo != NULL && OidIsValid(fcinfo->flinfo->fn_oid) &&
		func_volatile(fcinfo->flinfo->fn_oid) != PROVOLATILE_VOLATILE;
}

bool
sx_spi_exec(sx_call *call, const char *sql, size_t sql_len,
			int32 nparams, const sx_value *params, int64 limit,
			sx_spi_result **out)
{
	volatile bool ok = true;
	MemoryContext caller = CurrentMemoryContext;

	*out = NULL;
	if (sx_poisoned(call))
		return false;

	SX_GUARD(call, ok, {
		char	   *query = pnstrdup(sql, sql_len);
		Oid		   *types = NULL;
		Datum	   *values = NULL;
		char	   *nulls = NULL;
		sx_spi_result *res;
		int			rc;

		if (nparams < 0 || limit < 0)
			ereport(ERROR,
					(errcode(ERRCODE_INTERNAL_ERROR),
					 errmsg_internal("sutegi-pgext: bad SPI arguments")));
		if (nparams > 0)
		{
			types = palloc(sizeof(Oid) * nparams);
			values = palloc(sizeof(Datum) * nparams);
			nulls = palloc(nparams);
			for (int i = 0; i < nparams; i++)
			{
				types[i] = sx_kind_oid(params[i].kind);
				if (!OidIsValid(types[i]) || params[i].kind == SX_VOID)
					ereport(ERROR,
							(errcode(ERRCODE_INTERNAL_ERROR),
							 errmsg_internal("sutegi-pgext: bad SPI parameter kind %d", params[i].kind)));
				nulls[i] = params[i].isnull ? 'n' : ' ';
				values[i] = params[i].isnull ? (Datum) 0 : sx_to_datum(&params[i]);
			}
		}

		if (SPI_connect() != SPI_OK_CONNECT)
			ereport(ERROR,
					(errcode(ERRCODE_INTERNAL_ERROR),
					 errmsg_internal("sutegi-pgext: SPI_connect failed")));
		rc = SPI_execute_with_args(query, nparams, types, values, nulls,
								   sx_read_only(call->fcinfo), (long) limit);
		if (rc < 0)
			ereport(ERROR,
					(errcode(ERRCODE_INTERNAL_ERROR),
					 errmsg("sutegi-pgext: SPI_execute failed: %s", SPI_result_code_string(rc))));

		res = MemoryContextAllocZero(caller, sizeof(sx_spi_result));
		res->processed = SPI_processed;
		if (SPI_tuptable != NULL)
		{
			TupleDesc	desc = SPI_tuptable->tupdesc;

			res->nrows = SPI_processed;
			res->ncols = desc->natts;
			res->names = MemoryContextAllocZero(caller, mul_size(sizeof(char *), Max(desc->natts, 1)));
			for (int c = 0; c < desc->natts; c++)
				res->names[c] = MemoryContextStrdup(caller, NameStr(TupleDescAttr(desc, c)->attname));
			res->values = MemoryContextAllocHuge(caller,
												 mul_size(mul_size(sizeof(sx_value), Max(res->nrows, 1)),
														  Max(desc->natts, 1)));
			for (uint64 r = 0; r < res->nrows; r++)
			{
				HeapTuple	tuple = SPI_tuptable->vals[r];

				for (int c = 0; c < desc->natts; c++)
				{
					sx_value   *v = &res->values[r * desc->natts + c];
					char	   *text = SPI_getvalue(tuple, desc, c + 1);

					memset(v, 0, sizeof(*v));
					v->kind = SX_TEXT;
					if (text == NULL)
						v->isnull = 1;
					else
					{
						v->len = strlen(text);
						v->ptr = MemoryContextStrdup(caller, text);
						pfree(text);
					}
				}
			}
		}
		SPI_finish();
		*out = res;
	});
	return ok;
}
