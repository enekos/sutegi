//! First-class AI primitives.
//!
//! The thesis: a web app should be drivable by an LLM agent without a separate
//! integration layer. So sutegi treats **tools** as a native concept. You
//! implement the [`Tool`] trait; the framework exposes:
//!
//! * `GET  /__tools`        — an Anthropic-style tool manifest (name, description, input_schema)
//! * `POST /__tools/:name`  — invoke a tool with a JSON argument object
//!
//! Combined with `/__introspect`, an agent can discover both the app's HTTP
//! surface and its callable tools, then act — all over plain JSON.

use std::sync::Arc;

use sutegi_json::Json;
pub use sutegi_validate::{validate_schema, ValidationErrors};
use sutegi_web::{json, json_body, sse, App, Params, Request, Response, SseSink};

/// A callable unit of work an agent can invoke. The `parameters` schema is a
/// JSON Schema object describing the expected argument shape.
pub trait Tool: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Json;
    fn call(&self, args: Json) -> Result<Json, String>;
}

/// A tool that streams its output as Server-Sent Events — the shape an agent
/// uses for token-by-token LLM responses. Invoked at `POST /__tools/:name/stream`.
pub trait StreamTool: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters(&self) -> Json;
    /// Produce the stream. Push events via `sink` (each is flushed immediately).
    fn run(&self, args: Json, sink: &mut SseSink) -> std::io::Result<()>;
}

/// A collection of tools, exposable as a manifest and invokable by name.
#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
    stream_tools: Vec<Box<dyn StreamTool>>,
}

impl ToolRegistry {
    pub fn new() -> ToolRegistry {
        ToolRegistry {
            tools: Vec::new(),
            stream_tools: Vec::new(),
        }
    }

    pub fn add(mut self, tool: impl Tool) -> ToolRegistry {
        self.tools.push(Box::new(tool));
        self
    }

    /// Register a streaming (SSE) tool.
    pub fn add_stream(mut self, tool: impl StreamTool) -> ToolRegistry {
        self.stream_tools.push(Box::new(tool));
        self
    }

    /// Look up a tool by name.
    pub fn tool(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.iter().find(|t| t.name() == name).map(|b| b.as_ref())
    }

    /// Look up a streaming tool by name.
    pub fn stream_tool(&self, name: &str) -> Option<&dyn StreamTool> {
        self.stream_tools.iter().find(|t| t.name() == name).map(|b| b.as_ref())
    }

    /// One manifest entry per tool, in the `{name, description, input_schema,
    /// streaming}` shape that maps directly onto LLM tool-calling APIs. The
    /// `streaming` flag tells an agent to call `/__tools/:name/stream` (SSE)
    /// instead of `/__tools/:name`.
    pub fn schema_entries(&self) -> Vec<Json> {
        let mut entries: Vec<Json> = self
            .tools
            .iter()
            .map(|t| manifest_entry(t.name(), t.description(), t.parameters(), false))
            .collect();
        for t in &self.stream_tools {
            entries.push(manifest_entry(t.name(), t.description(), t.parameters(), true));
        }
        entries
    }

    /// The full manifest as a JSON array.
    pub fn manifest(&self) -> Json {
        Json::arr(self.schema_entries())
    }

    /// Invoke a tool by name, validating arguments against its full JSON
    /// Schema (`type`, `required`, `enum`, bounds, …) before dispatch. On
    /// validation failure, the error string is a compact JSON object so
    /// programmatic callers still get structured detail.
    pub fn call(&self, name: &str, args: Json) -> Result<Json, String> {
        let tool = self
            .tool(name)
            .ok_or_else(|| format!("unknown tool '{}'", name))?;
        if let Err(errs) = validate_schema(&tool.parameters(), &args) {
            return Err(errs.to_json().to_string());
        }
        tool.call(args)
    }
}

/// Mount the AI surface onto an app: registers tool schemas for introspection
/// and wires the `/__tools` endpoints.
pub fn mount(mut app: App, registry: ToolRegistry) -> App {
    for entry in registry.schema_entries() {
        app = app.register_tool(entry);
    }
    let registry = Arc::new(registry);

    let list = Arc::clone(&registry);
    app = app.get(
        "/__tools",
        "List callable AI tools as an LLM tool-calling manifest.",
        move |_req: &Request, _p: &Params| json(200, &list.manifest()),
    );

    let invoke = Arc::clone(&registry);
    app = app.post(
        "/__tools/:name",
        "Invoke an AI tool by name with a JSON argument object.",
        move |req: &Request, p: &Params| -> Response {
            let name = p.get("name").cloned().unwrap_or_default();
            let args = match json_body(req) {
                Ok(v) => v,
                Err(e) => {
                    return json(400, &Json::obj(vec![("error", Json::str(e))]));
                }
            };
            let tool = match invoke.tool(&name) {
                Some(t) => t,
                None => {
                    return json(
                        404,
                        &Json::obj(vec![("error", Json::str(format!("unknown tool '{}'", name)))]),
                    );
                }
            };
            // Type-aware validation against the tool's declared input schema.
            if let Err(errs) = validate_schema(&tool.parameters(), &args) {
                return json(
                    422,
                    &Json::obj(vec![
                        ("error", Json::str("validation failed")),
                        ("errors", errs.to_json()),
                    ]),
                );
            }
            match tool.call(args) {
                Ok(out) => json(200, &out),
                Err(e) => json(422, &Json::obj(vec![("error", Json::str(e))])),
            }
        },
    );

    // Streaming tool invocation over Server-Sent Events.
    let stream_reg = Arc::clone(&registry);
    app = app.post(
        "/__tools/:name/stream",
        "Invoke a streaming AI tool by name; response is text/event-stream (SSE).",
        move |req: &Request, p: &Params| -> Response {
            let name = p.get("name").cloned().unwrap_or_default();
            let args = match json_body(req) {
                Ok(v) => v,
                Err(e) => return json(400, &Json::obj(vec![("error", Json::str(e))])),
            };
            // Validate up front, while we can still send a normal JSON error.
            match stream_reg.stream_tool(&name) {
                None => {
                    return json(
                        404,
                        &Json::obj(vec![("error", Json::str(format!("unknown streaming tool '{}'", name)))]),
                    )
                }
                Some(tool) => {
                    if let Err(errs) = validate_schema(&tool.parameters(), &args) {
                        return json(
                            422,
                            &Json::obj(vec![
                                ("error", Json::str("validation failed")),
                                ("errors", errs.to_json()),
                            ]),
                        );
                    }
                }
            }
            // Hand off to the SSE stream; re-resolve the tool inside the producer.
            let reg = Arc::clone(&stream_reg);
            sse(move |sink: &mut SseSink| {
                if let Some(tool) = reg.stream_tool(&name) {
                    tool.run(args, sink)
                } else {
                    Ok(())
                }
            })
        },
    );

    app
}

fn manifest_entry(name: &str, description: &str, parameters: Json, streaming: bool) -> Json {
    Json::obj(vec![
        ("name", Json::str(name)),
        ("description", Json::str(description)),
        ("input_schema", parameters),
        ("streaming", Json::Bool(streaming)),
    ])
}

/// Helpers to construct JSON Schema fragments for tool `parameters`, so
/// declaring a schema reads declaratively instead of as nested map building.
pub mod schema {
    use sutegi_json::Json;

    pub fn string(description: &str) -> Json {
        Json::obj(vec![
            ("type", Json::str("string")),
            ("description", Json::str(description)),
        ])
    }

    pub fn integer(description: &str) -> Json {
        Json::obj(vec![
            ("type", Json::str("integer")),
            ("description", Json::str(description)),
        ])
    }

    pub fn boolean(description: &str) -> Json {
        Json::obj(vec![
            ("type", Json::str("boolean")),
            ("description", Json::str(description)),
        ])
    }

    /// An object schema from `(field, schema)` pairs and a list of required fields.
    pub fn object(properties: Vec<(&str, Json)>, required: &[&str]) -> Json {
        Json::obj(vec![
            ("type", Json::str("object")),
            ("properties", Json::obj(properties)),
            (
                "required",
                Json::arr(required.iter().map(|r| Json::str(*r)).collect()),
            ),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;
    impl Tool for Echo {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echo a message back."
        }
        fn parameters(&self) -> Json {
            schema::object(vec![("msg", schema::string("text to echo"))], &["msg"])
        }
        fn call(&self, args: Json) -> Result<Json, String> {
            let msg = args.get("msg").and_then(|j| j.as_str()).unwrap_or("");
            Ok(Json::obj(vec![("echo", Json::str(msg))]))
        }
    }

    #[test]
    fn manifest_has_input_schema() {
        let reg = ToolRegistry::new().add(Echo);
        let manifest = reg.manifest();
        if let Json::Arr(entries) = manifest {
            assert_eq!(entries[0].get("name").unwrap(), &Json::str("echo"));
            assert!(entries[0].get("input_schema").is_some());
        } else {
            panic!("manifest should be an array");
        }
    }

    #[test]
    fn missing_required_field_is_rejected() {
        let reg = ToolRegistry::new().add(Echo);
        let err = reg.call("echo", Json::obj(vec![])).unwrap_err();
        assert!(err.contains("msg"));
    }

    #[test]
    fn valid_call_succeeds() {
        let reg = ToolRegistry::new().add(Echo);
        let out = reg
            .call("echo", Json::obj(vec![("msg", Json::str("hi"))]))
            .unwrap();
        assert_eq!(out.get("echo").unwrap(), &Json::str("hi"));
    }
}
