use std::marker::PhantomData;

use sutegi_contract::schema::json_schema;
use sutegi_contract::{Contract, Cx, Kind, Procedure, Registry, Type};
use sutegi_json::Json;

use crate::{
    compile_pattern, json, sse, Ctx, Error, Handler, IntoResponse, Method, Mw, Route, Seg, SseSink,
};

#[derive(Clone, Debug)]
pub struct Endpoint {
    name: String,
    method: Method,
    pattern: String,
    doc: String,
}

impl Endpoint {
    pub fn new(method: Method, name: &str, pattern: &str) -> Endpoint {
        Endpoint {
            name: name.to_string(),
            method,
            pattern: pattern.to_string(),
            doc: String::new(),
        }
    }

    pub fn get(name: &str, pattern: &str) -> Endpoint {
        Endpoint::new(Method::Get, name, pattern)
    }

    pub fn post(name: &str, pattern: &str) -> Endpoint {
        Endpoint::new(Method::Post, name, pattern)
    }

    pub fn put(name: &str, pattern: &str) -> Endpoint {
        Endpoint::new(Method::Put, name, pattern)
    }

    pub fn delete(name: &str, pattern: &str) -> Endpoint {
        Endpoint::new(Method::Delete, name, pattern)
    }

    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    #[must_use]
    pub fn doc(mut self, doc: &str) -> Endpoint {
        self.doc = doc.to_string();
        self
    }
}

type StreamBody<E, O> =
    Box<dyn FnOnce(&mut Emitter<'_, '_, E>) -> Result<O, Error> + Send + 'static>;

pub struct Stream<E, O> {
    run: StreamBody<E, O>,
}

impl<E, O> Stream<E, O> {
    pub fn new(
        run: impl FnOnce(&mut Emitter<'_, '_, E>) -> Result<O, Error> + Send + 'static,
    ) -> Stream<E, O> {
        Stream { run: Box::new(run) }
    }
}

impl<E, O> std::fmt::Debug for Stream<E, O> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Stream")
    }
}

pub struct Emitter<'a, 'b, E> {
    sink: &'a mut SseSink<'b>,
    gone: bool,
    sent: u64,
    event: PhantomData<fn(&E)>,
}

impl<E> std::fmt::Debug for Emitter<'_, '_, E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Emitter")
            .field("gone", &self.gone)
            .field("sent", &self.sent)
            .finish()
    }
}

impl<E: Contract> Emitter<'_, '_, E> {
    pub fn emit(&mut self, event: &E) -> Result<(), Error> {
        let frame = event.encode().to_string();
        self.write(|sink| sink.event("item", &frame))?;
        self.sent = self.sent.saturating_add(1);
        Ok(())
    }

    pub fn heartbeat(&mut self) -> Result<(), Error> {
        self.write(|sink| sink.comment("heartbeat"))
    }

    pub fn sent(&self) -> u64 {
        self.sent
    }

    pub fn is_gone(&self) -> bool {
        self.gone
    }

    fn write(
        &mut self,
        f: impl FnOnce(&mut SseSink<'_>) -> std::io::Result<()>,
    ) -> Result<(), Error> {
        if self.gone {
            return Err(disconnected());
        }
        f(self.sink).map_err(|_| {
            self.gone = true;
            disconnected()
        })
    }
}

fn disconnected() -> Error {
    Error::new(499, "client disconnected")
}

fn pattern_params(pattern: &str) -> Vec<String> {
    compile_pattern(pattern)
        .into_iter()
        .filter_map(|s| match s {
            Seg::Param(n) | Seg::Rest(n) => Some(n),
            Seg::Lit(_) => None,
        })
        .collect()
}

struct Plan {
    procedure: Procedure,
    meta: Json,
}

fn plan(
    reg: &mut Registry,
    ep: &Endpoint,
    full_pattern: &str,
    kind: Kind,
    input: Type,
    output: Type,
    event: Option<Type>,
) -> Plan {
    let params = pattern_params(full_pattern);
    let fields = reg.field_names(&input);
    let object_like = matches!(input, Type::Unit | Type::Any) || fields.is_some();
    assert!(
        ep.method != Method::Get || object_like,
        "typed GET {:?} ({}): its input must be a struct (or ()), since it travels in the query string",
        ep.name,
        full_pattern
    );
    assert!(
        params.is_empty() || object_like,
        "typed route {:?} ({}): a path with parameters needs a struct (or ()) input to bind them into",
        ep.name,
        full_pattern
    );
    let bound: Vec<String> = match &fields {
        Some(fs) => params.iter().filter(|p| fs.contains(p)).cloned().collect(),
        None => Vec::new(),
    };
    let kind = match (kind, ep.method) {
        (Kind::Stream, _) => Kind::Stream,
        (_, Method::Get) => Kind::Query,
        _ => Kind::Mutation,
    };
    let procedure = Procedure {
        name: ep.name.clone(),
        kind,
        method: ep.method.as_str().to_string(),
        path: full_pattern.to_string(),
        doc: ep.doc.clone(),
        input,
        output,
        event,
        path_params: params,
        bound_params: bound,
    };
    let mut meta = vec![
        ("name", Json::str(procedure.name.clone())),
        ("kind", Json::str(procedure.kind.as_str())),
        ("input_schema", json_schema(reg, &procedure.input)),
        ("output_schema", json_schema(reg, &procedure.output)),
    ];
    if let Some(e) = &procedure.event {
        meta.push(("event_schema", json_schema(reg, e)));
    }
    reg.add_procedure(procedure.clone());
    Plan {
        procedure,
        meta: Json::obj(meta),
    }
}

fn decode_input<I: Contract>(c: &Ctx, from_query: bool, bound: &[String]) -> Result<I, Error> {
    let (value, cx) = if from_query {
        let m = c
            .query()
            .into_iter()
            .map(|(k, v)| (k, Json::Str(v)))
            .collect();
        (Json::Obj(m), Cx::lenient())
    } else {
        (c.json()?, Cx::lenient_keys(bound.iter().cloned()))
    };
    let value = match value {
        Json::Obj(mut m) if !bound.is_empty() => {
            for p in bound {
                if let Some(v) = c.param(p) {
                    m.insert(p.clone(), Json::Str(v.to_string()));
                }
            }
            Json::Obj(m)
        }
        other => other,
    };
    sutegi_contract::decode_with(&value, cx)
        .map_err(|errs| Error::unprocessable("validation failed").with_fields(errs.to_json()))
}

pub(crate) fn unary_route<I, O, H>(
    reg: &mut Registry,
    ep: Endpoint,
    full_pattern: String,
    middleware: Vec<Mw>,
    handler: H,
) -> Route
where
    I: Contract + 'static,
    O: Contract + 'static,
    H: Fn(&Ctx, I) -> Result<O, Error> + Send + Sync + 'static,
{
    let input = I::describe(reg);
    let output = O::describe(reg);
    let plan = plan(reg, &ep, &full_pattern, Kind::Query, input, output, None);
    let from_query = ep.method == Method::Get;
    let bound = plan.procedure.bound_params;
    let run: Handler = Box::new(move |c: &Ctx| {
        let input = match decode_input::<I>(c, from_query, &bound) {
            Ok(i) => i,
            Err(e) => return e.into_response(),
        };
        match handler(c, input) {
            Ok(out) => json(200, &out.encode()),
            Err(e) => e.into_response(),
        }
    });
    Route {
        method: ep.method,
        segs: compile_pattern(&full_pattern),
        pattern: full_pattern,
        doc: ep.doc,
        handler: run,
        middleware,
        contract: Some(plan.meta),
    }
}

pub(crate) fn stream_route<I, E, O, H>(
    reg: &mut Registry,
    ep: Endpoint,
    full_pattern: String,
    middleware: Vec<Mw>,
    handler: H,
) -> Route
where
    I: Contract + 'static,
    E: Contract + 'static,
    O: Contract + 'static,
    H: Fn(&Ctx, I) -> Result<Stream<E, O>, Error> + Send + Sync + 'static,
{
    assert!(
        ep.method != Method::Get,
        "typed stream {:?}: use POST (or PUT/DELETE), a stream's input travels in the body",
        ep.name
    );
    let input = I::describe(reg);
    let output = O::describe(reg);
    let event = E::describe(reg);
    let plan = plan(
        reg,
        &ep,
        &full_pattern,
        Kind::Stream,
        input,
        output,
        Some(event),
    );
    let bound = plan.procedure.bound_params;
    let run: Handler = Box::new(move |c: &Ctx| {
        let input = match decode_input::<I>(c, false, &bound) {
            Ok(i) => i,
            Err(e) => return e.into_response(),
        };
        let stream = match handler(c, input) {
            Ok(s) => s,
            Err(e) => return e.into_response(),
        };
        sse(move |sink| {
            let (result, gone) = {
                let mut emitter = Emitter {
                    sink: &mut *sink,
                    gone: false,
                    sent: 0,
                    event: PhantomData,
                };
                let result = (stream.run)(&mut emitter);
                (result, emitter.gone)
            };
            if gone {
                return Ok(());
            }
            match result {
                Ok(out) => sink.event("done", &out.encode().to_string()),
                Err(e) => {
                    let (status, body) = e.into_body();
                    let body = match body {
                        Json::Obj(mut m) => {
                            m.insert("status".to_string(), Json::Num(f64::from(status)));
                            Json::Obj(m)
                        }
                        other => other,
                    };
                    sink.event("error", &body.to_string())
                }
            }
        })
    });
    Route {
        method: ep.method,
        segs: compile_pattern(&full_pattern),
        pattern: full_pattern,
        doc: ep.doc,
        handler: run,
        middleware,
        contract: Some(plan.meta),
    }
}
