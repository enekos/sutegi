# Driving a sutegi app as an AI agent

Every sutegi app is self-describing over plain JSON/HTTP. You do not need source
access, an SDK, or any integration code. The loop is: **discover → manifest →
invoke**.

## 1. Discover the surface

```
GET /__introspect
```

Returns the full application surface:

```json
{
  "framework": "sutegi",
  "version": "0.1.0",
  "name": "todo-demo",
  "routes": [ { "method": "GET", "pattern": "/todos", "doc": "List all todos." } ],
  "models": [ { "table": "todos", "columns": [ { "name": "id", "type": "integer", "primary": true } ] } ],
  "tools":  [ { "name": "create_todo", "description": "...", "input_schema": { ... } } ]
}
```

`routes` is the HTTP surface (with intent in `doc`), `models` is the data shape,
`tools` is what you can call. Keys are sorted, so the document is stable to diff
and cache.

## 2. Get the tool manifest

```
GET /__tools
```

Returns an array of `{ name, description, input_schema }` — the exact shape
expected by LLM tool-calling APIs (Anthropic-style). Feed it straight into your
tool definitions.

## 3. Invoke a tool

```
POST /__tools/<name>
Content-Type: application/json

{ "title": "ship sutegi" }
```

- `200` → the tool's JSON result.
- `422 { "error": "validation failed", "errors": { "<field>": ["..."] } }` →
  arguments failed the tool's JSON Schema (type, `required`, `enum`, bounds).
  The `errors` map is keyed by dotted field path — fix those fields and retry.
- `404 { "error": "unknown tool '<name>'" }` → no such tool.
- `400 { "error": "..." }` → the body was not valid JSON.

## 4. Streaming tools (SSE)

Manifest entries with `"streaming": true` are invoked at a different endpoint
and return a Server-Sent Events stream instead of a single JSON body:

```
POST /__tools/<name>/stream
Content-Type: application/json

{ "prompt": "…" }
```

- Response is `text/event-stream`; read it incrementally. Frames look like
  `data: <chunk>\n\n`, with a final `event: done` frame.
- Validation still runs first: a bad argument object returns a normal JSON
  `422` *before* the stream opens, so you can distinguish "rejected" from
  "stream ended".

Non-streaming tools (`"streaming": false`) use `POST /__tools/<name>` and return
one JSON result, as in §3.

## Conventions you can rely on

- All framework endpoints are namespaced under `/__`.
- All errors are `{ "error": string }`.
- Tool argument schemas are real JSON Schema objects; honor `required`.
- Numbers serialize without trailing `.0` when integral.

That is the entire contract.
