use sutegi_contract::schema::json_schema;
use sutegi_contract::{ChannelSpec, Contract, Message, Registry};
use sutegi_json::Json;

use crate::channel::{Channel, Reply};
use crate::hub::{ChannelHub, Socket};

#[derive(Clone, Debug, Default)]
pub(crate) struct ChannelContract {
    reg: Registry,
    join: Option<Message>,
    handles: Vec<Message>,
    emits: Vec<Message>,
}

impl ChannelContract {
    fn is_typed(&self) -> bool {
        self.join.is_some() || !self.handles.is_empty() || !self.emits.is_empty()
    }
}

fn refusal(error: &str, errors: Option<Json>) -> Json {
    let mut out = vec![("error", Json::str(error))];
    if let Some(e) = errors {
        out.push(("errors", e));
    }
    Json::obj(out)
}

fn decode<I: Contract>(payload: &Json) -> Result<I, Json> {
    sutegi_contract::decode(payload).map_err(|e| refusal("validation failed", Some(e.to_json())))
}

impl Channel {
    pub fn typed_join<I, O>(
        mut self,
        doc: &str,
        f: impl Fn(&Socket, I) -> Result<O, String> + Send + Sync + 'static,
    ) -> Channel
    where
        I: Contract + 'static,
        O: Contract + 'static,
    {
        let payload = I::describe(&mut self.contract.reg);
        let reply = O::describe(&mut self.contract.reg);
        let schema = json_schema(&self.contract.reg, &payload);
        self.contract.join = Some(Message {
            event: "stg:join".to_string(),
            doc: doc.to_string(),
            payload,
            reply: Some(reply),
        });
        self.join_schema(doc, schema)
            .on_join(move |socket, payload| {
                let input = decode::<I>(payload)?;
                f(socket, input)
                    .map(|o| o.encode())
                    .map_err(|e| refusal(&e, None))
            })
    }

    pub fn typed_on<I, O>(
        mut self,
        event: &str,
        doc: &str,
        f: impl Fn(&Socket, I) -> Result<O, String> + Send + Sync + 'static,
    ) -> Channel
    where
        I: Contract + 'static,
        O: Contract + 'static,
    {
        let payload = I::describe(&mut self.contract.reg);
        let reply = O::describe(&mut self.contract.reg);
        let schema = json_schema(&self.contract.reg, &payload);
        assert!(
            !self.contract.handles.iter().any(|m| m.event == event),
            "event {event:?} is already typed on channel {:?}",
            self.pattern
        );
        self.contract.handles.push(Message {
            event: event.to_string(),
            doc: doc.to_string(),
            payload,
            reply: Some(reply),
        });
        self.on(event, move |socket, payload| match decode::<I>(payload) {
            Err(e) => Reply::Err(e),
            Ok(input) => match f(socket, input) {
                Ok(o) => Reply::Ok(o.encode()),
                Err(e) => Reply::Err(refusal(&e, None)),
            },
        })
        .event_schema(event, doc, schema)
    }

    pub fn typed_emits<T: Contract>(mut self, event: &str, doc: &str) -> Channel {
        let payload = T::describe(&mut self.contract.reg);
        let schema = json_schema(&self.contract.reg, &payload);
        assert!(
            !self.contract.emits.iter().any(|m| m.event == event),
            "event {event:?} is already declared as emitted on channel {:?}",
            self.pattern
        );
        self.contract.emits.push(Message {
            event: event.to_string(),
            doc: doc.to_string(),
            payload,
            reply: None,
        });
        self.emits(event, doc, schema)
    }

    fn contract_spec(&self) -> Option<(Registry, ChannelSpec)> {
        if !self.contract.is_typed() {
            return None;
        }
        let spec = ChannelSpec {
            pattern: self.pattern.clone(),
            doc: self.doc.clone(),
            join: self.contract.join.clone(),
            handles: self.contract.handles.clone(),
            emits: self.contract.emits.clone(),
        };
        Some((self.contract.reg.clone(), spec))
    }
}

impl Socket {
    pub fn push_typed<T: Contract>(&self, event: &str, payload: &T) {
        self.push(event, &payload.encode());
    }

    pub fn broadcast_typed<T: Contract>(&self, event: &str, payload: &T) {
        self.broadcast(event, &payload.encode());
    }

    pub fn broadcast_from_typed<T: Contract>(&self, event: &str, payload: &T) {
        self.broadcast_from(event, &payload.encode());
    }
}

impl ChannelHub {
    pub fn broadcast_typed<T: Contract>(&self, topic: &str, event: &str, payload: &T) {
        self.broadcast(topic, event, &payload.encode());
    }

    pub fn contract(&self) -> Registry {
        let mut out = Registry::new();
        for ch in &self.inner.channels {
            if let Some((reg, spec)) = ch.contract_spec() {
                out.absorb(reg);
                out.add_channel(spec);
            }
        }
        out
    }
}
