#![cfg(feature = "contract")]

use sutegi::contract::{decode, decode_lenient, schema, type_of, Shape, Type};
use sutegi::http::Body;
use sutegi::prelude::*;

/// An agent the workspace can run.
#[derive(Contract, Debug, Clone, PartialEq)]
struct Agent {
    id: AgentId,
    #[contract(min_len = 1, max_len = 40)]
    name: String,
    /// Shown under the name.
    description: Option<String>,
    approval: Approval,
    #[contract(default, min = 1, max = 50)]
    max_steps: u32,
    #[contract(skip)]
    cached: Option<u8>,
}

#[derive(Contract, Debug, Clone, PartialEq)]
struct AgentId(String);

#[derive(Contract, Debug, Clone, Copy, PartialEq)]
enum Approval {
    Always,
    WritesOnly,
    #[contract(rename = "off")]
    Never,
}

#[derive(Contract, Debug, Clone, PartialEq)]
#[contract(tag = "t")]
enum AgentEvent {
    Step { n: u32, tool: Option<String> },
    Token { text: String },
    Usage(Usage),
    Idle,
}

#[derive(Contract, Debug, Clone, PartialEq)]
struct Usage {
    tokens: u64,
}

#[derive(Contract, Debug, Clone, PartialEq)]
struct Page<T> {
    items: Vec<T>,
    next: Option<String>,
}

#[derive(Contract, Debug, Clone, PartialEq)]
struct Tree {
    label: String,
    children: Vec<Tree>,
}

#[derive(Contract, Debug)]
struct ListAgents {
    ws: String,
    #[contract(default)]
    limit: u32,
    approval: Option<Approval>,
    tags: Option<Vec<String>>,
}

#[derive(Contract, Debug)]
struct GetAgent {
    id: u64,
}

#[derive(Contract, Debug)]
struct CreateAgent {
    #[contract(min_len = 1)]
    name: String,
    approval: Approval,
}

#[derive(Contract, Debug)]
struct RunAgent {
    id: u64,
    message: String,
}

#[derive(Contract, Debug)]
struct RunSummary {
    steps: u32,
    reply: String,
}

fn agent(id: &str) -> Agent {
    Agent {
        id: AgentId(id.to_string()),
        name: "Scout".into(),
        description: None,
        approval: Approval::WritesOnly,
        max_steps: 5,
        cached: None,
    }
}

#[test]
fn struct_round_trips_and_skips() {
    let a = Agent {
        cached: Some(9),
        ..agent("a1")
    };
    let j = a.encode();
    assert_eq!(j["approval"], Json::str("writes_only"));
    assert_eq!(j["id"], Json::str("a1"));
    assert!(j.get("cached").is_none());
    let back: Agent = decode(&j).unwrap();
    assert_eq!(back, Agent { cached: None, ..a });
}

#[test]
fn missing_option_and_default_fields_are_fine() {
    let j = Json::parse(r#"{"id":"x","name":"n","approval":"off"}"#).unwrap();
    let a: Agent = decode(&j).unwrap();
    assert_eq!(a.description, None);
    assert_eq!(a.max_steps, 0);
    assert_eq!(a.approval, Approval::Never);
}

#[test]
fn errors_are_keyed_by_path_and_all_reported() {
    let j = Json::parse(r#"{"id":5,"name":"","approval":"sometimes","max_steps":99}"#).unwrap();
    let errs = decode::<Agent>(&j).unwrap_err();
    assert_eq!(errs.get("id").unwrap(), ["expected a string"]);
    assert_eq!(
        errs.get("name").unwrap(),
        ["must have at least 1 characters"]
    );
    assert!(errs.get("approval").unwrap()[0].contains("\"writes_only\""));
    assert_eq!(errs.get("max_steps").unwrap(), ["must be <= 50"]);
}

#[test]
fn required_field_missing_is_named() {
    let errs = decode::<Usage>(&Json::parse("{}").unwrap()).unwrap_err();
    assert_eq!(
        errs.get("tokens").unwrap(),
        ["expected a value: field is required"]
    );
}

#[test]
fn nested_errors_carry_the_full_path() {
    let j = Json::parse(
        r#"{"label":"a","children":[{"label":"b","children":[{"label":3,"children":[]}]}]}"#,
    )
    .unwrap();
    let errs = decode::<Tree>(&j).unwrap_err();
    assert_eq!(
        errs.get("children[0].children[0].label").unwrap(),
        ["expected a string"]
    );
}

#[test]
fn tagged_union_round_trips_every_variant_shape() {
    for ev in [
        AgentEvent::Step {
            n: 2,
            tool: Some("sql".into()),
        },
        AgentEvent::Token { text: "hi".into() },
        AgentEvent::Usage(Usage { tokens: 12 }),
        AgentEvent::Idle,
    ] {
        let j = ev.encode();
        assert!(j["t"].as_str().is_some(), "{j}");
        assert_eq!(decode::<AgentEvent>(&j).unwrap(), ev);
    }
    assert_eq!(
        AgentEvent::Usage(Usage { tokens: 1 }).encode().to_string(),
        r#"{"t":"usage","tokens":1}"#
    );
    let errs = decode::<AgentEvent>(&Json::parse(r#"{"t":"nope"}"#).unwrap()).unwrap_err();
    assert!(errs.get("t").unwrap()[0].starts_with("expected one of"));
}

#[test]
fn generics_are_monomorphised_by_name() {
    let (reg, ty) = type_of::<Page<Agent>>();
    assert_eq!(ty, Type::Ref("Page_Agent".into()));
    assert!(reg.def("Agent").is_some());
    let (_, ty) = type_of::<Page<String>>();
    assert_eq!(ty, Type::Ref("Page_String".into()));
}

#[test]
fn recursive_types_describe_and_render() {
    let (reg, ty) = type_of::<Tree>();
    let Shape::Struct(fields) = &reg.def("Tree").unwrap().shape else {
        panic!()
    };
    assert_eq!(fields[1].ty, Type::list(Type::Ref("Tree".into())));
    let s = schema::json_schema(&reg, &ty);
    assert!(s.to_string().contains("recursive"), "{s}");
}

#[test]
fn json_schema_carries_rules_docs_and_requiredness() {
    let (reg, ty) = type_of::<Agent>();
    let s = schema::json_schema(&reg, &ty);
    assert_eq!(
        s["description"],
        Json::str("An agent the workspace can run.")
    );
    assert_eq!(s["properties"]["name"]["minLength"], Json::Num(1.0));
    assert_eq!(
        s["properties"]["description"]["type"],
        Json::arr(vec![Json::str("string"), Json::str("null")])
    );
    assert_eq!(
        s["properties"]["description"]["description"],
        Json::str("Shown under the name.")
    );
    assert_eq!(
        s["properties"]["approval"]["enum"],
        Json::arr(vec![
            Json::str("always"),
            Json::str("writes_only"),
            Json::str("off")
        ])
    );
    let required: Vec<&str> = s["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Json::as_str)
        .collect();
    assert_eq!(required, ["id", "name", "approval"]);
    #[cfg(feature = "validate")]
    {
        let bad = Json::parse(r#"{"id":"x","name":"","approval":"off"}"#).unwrap();
        assert!(validate_schema(&s, &bad).is_err());
    }
}

#[test]
#[should_panic(expected = "is used by both")]
fn two_rust_types_with_one_wire_name_panic() {
    mod a {
        #[derive(sutegi::Contract)]
        pub struct Same {
            pub x: u8,
        }
    }
    mod b {
        #[derive(sutegi::Contract)]
        pub struct Same {
            pub y: u8,
        }
    }
    #[derive(Contract)]
    struct Both {
        a: a::Same,
        b: b::Same,
    }
    let _ = type_of::<Both>();
}

#[test]
fn lenient_decoding_reads_query_strings() {
    let j = Json::parse(r#"{"ws":"w1","limit":"10","approval":"always","tags":"[\"a\",\"b\"]"}"#)
        .unwrap();
    let q: ListAgents = decode_lenient(&j).unwrap();
    assert_eq!(q.limit, 10);
    assert_eq!(q.tags, Some(vec!["a".to_string(), "b".to_string()]));
    assert!(decode::<ListAgents>(&j).is_err());
}

fn app() -> App {
    App::new("contract-test")
        .typed(
            Endpoint::get("agents.list", "/w/:ws/agents").doc("Agents in a workspace."),
            |_c: &Ctx, q: ListAgents| -> Result<Page<Agent>, Error> {
                let n = if q.limit == 0 { 2 } else { q.limit as usize };
                Ok(Page {
                    items: (0..n).map(|i| agent(&format!("{}-{i}", q.ws))).collect(),
                    next: q.approval.map(|a| a.encode().to_string()),
                })
            },
        )
        .typed(
            Endpoint::get("agents.get", "/agents/:id"),
            |_c: &Ctx, q: GetAgent| -> Result<Agent, Error> {
                if q.id == 404 {
                    return Err(Error::not_found("no such agent"));
                }
                Ok(agent(&q.id.to_string()))
            },
        )
        .group("/api", vec![], |g| {
            g.typed(
                Endpoint::post("agents.create", "/agents").doc("Create an agent."),
                |_c: &Ctx, b: CreateAgent| -> Result<Agent, Error> {
                    Ok(Agent {
                        name: b.name,
                        approval: b.approval,
                        ..agent("new")
                    })
                },
            )
            .typed(
                Endpoint::delete("agents.remove", "/agents/:id"),
                |c: &Ctx, (): ()| -> Result<(), Error> {
                    assert!(c.param("id").is_some());
                    Ok(())
                },
            )
            .typed_stream(
                Endpoint::post("agents.run", "/agents/:id/run"),
                |_c: &Ctx, r: RunAgent| -> Result<Stream<AgentEvent, RunSummary>, Error> {
                    if r.message.is_empty() {
                        return Err(Error::unprocessable("say something"));
                    }
                    let fail = r.message == "fail";
                    Ok(Stream::new(move |out| {
                        out.emit(&AgentEvent::Step { n: 1, tool: None })?;
                        for w in r.message.split(' ') {
                            out.emit(&AgentEvent::Token {
                                text: w.to_string(),
                            })?;
                        }
                        if fail {
                            return Err(Error::bad_request("the model refused"));
                        }
                        Ok(RunSummary {
                            steps: u32::try_from(out.sent()).unwrap_or(u32::MAX),
                            reply: format!("ran {}", r.id),
                        })
                    }))
                },
            )
        })
}

fn request(method: Method, target: &str, body: &str) -> Request {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    Request {
        method,
        path: path.to_string(),
        query: query.to_string(),
        version: "HTTP/1.1".into(),
        headers: vec![("content-type".into(), "application/json".into())],
        body: body.as_bytes().to_vec(),
        peer: None,
    }
}

fn body_of(resp: Response) -> String {
    match resp.body {
        Body::Full(b) => String::from_utf8(b).unwrap(),
        Body::Stream(producer) => {
            let mut out: Vec<u8> = Vec::new();
            producer(&mut out).unwrap();
            String::from_utf8(out).unwrap()
        }
        Body::Upgrade(_) => panic!("unexpected upgrade"),
    }
}

fn call(
    svc: &impl Fn(Request) -> Response,
    method: Method,
    target: &str,
    body: &str,
) -> (u16, Json) {
    let resp = svc(request(method, target, body));
    let status = resp.status;
    let text = body_of(resp);
    (status, Json::parse(&text).unwrap_or(Json::Str(text)))
}

#[test]
fn typed_get_binds_path_and_query() {
    let svc = app().service();
    let (s, j) = call(&svc, Method::Get, "/w/acme/agents?limit=3&approval=off", "");
    assert_eq!(s, 200, "{j}");
    assert_eq!(j["items"].as_array().unwrap().len(), 3);
    assert_eq!(j["items"][0]["id"], Json::str("acme-0"));
    assert_eq!(j["next"], Json::str("\"off\""));

    let (s, j) = call(&svc, Method::Get, "/agents/7", "");
    assert_eq!(s, 200, "{j}");
    assert_eq!(j["id"], Json::str("7"));
}

#[test]
fn typed_routes_answer_422_with_field_errors() {
    let svc = app().service();
    let (s, j) = call(&svc, Method::Get, "/agents/seven", "");
    assert_eq!(s, 422);
    assert_eq!(
        j["errors"]["id"],
        Json::arr(vec![Json::str("expected a number")])
    );

    let (s, j) = call(
        &svc,
        Method::Post,
        "/api/agents",
        r#"{"name":"","approval":"maybe"}"#,
    );
    assert_eq!(s, 422);
    assert_eq!(j["error"], Json::str("validation failed"));
    assert!(
        j["errors"].get("name").is_some() && j["errors"].get("approval").is_some(),
        "{j}"
    );

    let (s, _) = call(&svc, Method::Post, "/api/agents", "{not json");
    assert_eq!(s, 400);

    let (s, j) = call(&svc, Method::Get, "/agents/404", "");
    assert_eq!((s, j["error"].as_str()), (404, Some("no such agent")));
}

#[test]
fn typed_mutation_and_unit_output() {
    let svc = app().service();
    let (s, j) = call(
        &svc,
        Method::Post,
        "/api/agents",
        r#"{"name":"Ada","approval":"always","extra":1}"#,
    );
    assert_eq!(s, 200, "{j}");
    assert_eq!(j["name"], Json::str("Ada"));
    let (s, j) = call(&svc, Method::Delete, "/api/agents/3", "");
    assert_eq!((s, j), (200, Json::Null));
}

#[test]
fn path_params_win_over_the_body() {
    let svc = app().service();
    let (_, j) = call(
        &svc,
        Method::Post,
        "/api/agents/5/run",
        r#"{"id":999,"message":"a b"}"#,
    );
    let text = j.as_str().unwrap().to_string();
    assert!(text.contains(r#""reply":"ran 5""#), "{text}");
}

fn frames(text: &str) -> Vec<(String, Json)> {
    text.split("\n\n")
        .filter(|f| !f.trim().is_empty())
        .map(|f| {
            let mut event = "message".to_string();
            let mut data = String::new();
            for line in f.lines() {
                if let Some(e) = line.strip_prefix("event: ") {
                    event = e.to_string();
                } else if let Some(d) = line.strip_prefix("data: ") {
                    data.push_str(d);
                }
            }
            (event, Json::parse(&data).unwrap())
        })
        .collect()
}

#[test]
fn typed_stream_emits_items_then_done() {
    let svc = app().service();
    let resp = svc(request(
        Method::Post,
        "/api/agents/9/run",
        r#"{"message":"hello there"}"#,
    ));
    assert_eq!(resp.status, 200);
    assert!(resp
        .headers
        .iter()
        .any(|(k, v)| k == "content-type" && v == "text/event-stream"));
    let f = frames(&body_of(resp));
    let kinds: Vec<&str> = f.iter().map(|(e, _)| e.as_str()).collect();
    assert_eq!(kinds, ["item", "item", "item", "done"]);
    assert_eq!(f[0].1, AgentEvent::Step { n: 1, tool: None }.encode());
    assert_eq!(f[3].1["steps"], Json::Num(3.0));
    assert_eq!(f[3].1["reply"], Json::str("ran 9"));
}

#[test]
fn typed_stream_rejects_before_opening_and_reports_errors_in_band() {
    let svc = app().service();
    let (s, j) = call(&svc, Method::Post, "/api/agents/9/run", r#"{"message":""}"#);
    assert_eq!((s, j["error"].as_str()), (422, Some("say something")));
    let (s, j) = call(&svc, Method::Post, "/api/agents/9/run", "{}");
    assert_eq!(s, 422);
    assert!(j["errors"].get("message").is_some());

    let resp = svc(request(
        Method::Post,
        "/api/agents/9/run",
        r#"{"message":"fail"}"#,
    ));
    let f = frames(&body_of(resp));
    let last = f.last().unwrap();
    assert_eq!(last.0, "error");
    assert_eq!(last.1["status"], Json::Num(400.0));
    assert_eq!(last.1["error"], Json::str("the model refused"));
}

#[test]
fn stream_stops_quietly_when_the_client_goes_away() {
    struct Broken;
    impl std::io::Write for Broken {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let svc = app().service();
    let resp = svc(request(
        Method::Post,
        "/api/agents/9/run",
        r#"{"message":"a"}"#,
    ));
    let Body::Stream(producer) = resp.body else {
        panic!()
    };
    assert!(producer(&mut Broken).is_ok());
}

#[test]
fn introspection_and_contract_endpoints() {
    let svc = app().service();
    let (_, intro) = call(&svc, Method::Get, "/__introspect", "");
    let routes = intro["routes"].as_array().unwrap();
    let run = routes
        .iter()
        .find(|r| r["pattern"] == Json::str("/api/agents/:id/run"))
        .unwrap();
    assert_eq!(run["contract"]["name"], Json::str("agents.run"));
    assert_eq!(run["contract"]["kind"], Json::str("stream"));
    assert!(run["contract"]["event_schema"].get("oneOf").is_some());
    assert_eq!(intro["contract"]["typescript"], Json::str("/__contract.ts"));

    let (s, m) = call(&svc, Method::Get, "/__contract", "");
    assert_eq!(s, 200);
    let names: Vec<&str> = m["procedures"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert_eq!(
        names,
        [
            "agents.list",
            "agents.get",
            "agents.create",
            "agents.remove",
            "agents.run"
        ]
    );
    assert!(m["types"].get("Page_Agent").is_some());

    let resp = svc(request(Method::Get, "/__contract.ts", ""));
    assert_eq!(resp.status, 200);
    assert!(body_of(resp).contains("export function createClient"));
}

#[test]
fn typescript_client_shape() {
    let out = app().contract_ts();
    for needle in [
        "export interface Agent {",
        "  /** Shown under the name. */\n  description?: string | null;",
        "  max_steps?: number;",
        "export type AgentId = string;",
        "export type Approval = \"always\" | \"writes_only\" | \"off\";",
        "export const ApprovalValues = [\"always\", \"writes_only\", \"off\"] as const;",
        "  | { t: \"step\"; n: number; tool?: string | null; }",
        "  | ({ t: \"usage\" } & Usage)",
        "  | { t: \"idle\" };",
        "export interface Page_Agent {",
        "  items: Agent[];",
        "list: (input: ListAgents): Promise<Page_Agent> => rt.call<Page_Agent>(\"GET\", \"/w/:ws/agents\", [\"ws\"], input)",
        "remove: (input: { id: string | number; }): Promise<null> => rt.call<null>(\"DELETE\", \"/api/agents/:id\", [\"id\"], input)",
        "run: (input: RunAgent): Streamed<AgentEvent, RunSummary> => rt.stream<AgentEvent, RunSummary>(\"POST\", \"/api/agents/:id/run\", [\"id\"], input)",
        "export type Client = ReturnType<typeof createClient>;",
    ] {
        assert!(out.contains(needle), "missing {needle:?} in:\n{out}");
    }
    assert!(!out.contains("cached"));
    assert!(!out.contains("export const channels"));
}

#[test]
#[should_panic(expected = "input must be a struct")]
fn get_with_a_non_struct_input_is_refused() {
    let _ = App::new("x").typed(
        Endpoint::get("bad", "/bad"),
        |_c: &Ctx, v: Vec<u8>| -> Result<usize, Error> { Ok(v.len()) },
    );
}

#[test]
#[should_panic(expected = "collides")]
fn duplicate_procedure_names_are_refused() {
    let h = |_c: &Ctx, (): ()| -> Result<(), Error> { Ok(()) };
    let _ = App::new("x")
        .typed(Endpoint::get("a.b", "/1"), h)
        .typed(Endpoint::get("a.b", "/2"), h);
}

#[test]
fn snapshot_check_flags_drift() {
    let dir = std::env::temp_dir().join(format!("sutegi-contract-it-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("api.ts");
    let rendered = app().contract_ts();
    std::fs::write(&file, &rendered).unwrap();
    assert!(sutegi::contract::snapshot::check_file(&file, &rendered).is_ok());
    let changed = App::new("x")
        .typed(
            Endpoint::get("only", "/only"),
            |_c: &Ctx, (): ()| -> Result<u8, Error> { Ok(1) },
        )
        .contract_ts();
    assert!(sutegi::contract::snapshot::check_file(&file, &changed).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}
