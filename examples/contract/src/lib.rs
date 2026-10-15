use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use sutegi::prelude::*;

#[derive(Contract, Clone, Debug)]
pub struct NoteId(pub u64);

#[derive(Contract, Clone, Debug)]
#[contract(doc = "A note in the notebook.")]
pub struct Note {
    pub id: NoteId,
    pub title: String,
    pub body: String,
    pub tags: Vec<String>,
    pub pinned: bool,
}

#[derive(Contract, Debug)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u32,
}

#[derive(Contract, Debug)]
pub struct ListNotes {
    pub tag: Option<String>,
    #[contract(default, max = 100)]
    pub limit: u32,
}

#[derive(Contract, Debug)]
pub struct NoteRef {
    pub id: u64,
}

#[derive(Contract, Debug)]
pub struct NewNote {
    #[contract(min_len = 1, max_len = 80)]
    pub title: String,
    #[contract(default)]
    pub body: String,
    #[contract(default, max_len = 8)]
    pub tags: Vec<String>,
}

#[derive(Contract, Debug)]
pub struct Summarize {
    pub id: u64,
    #[contract(default, max = 20)]
    pub words: u32,
}

#[derive(Contract, Debug)]
#[contract(tag = "t")]
pub enum SummaryEvent {
    Started { words: u32 },
    Word { text: String },
}

#[derive(Contract, Debug)]
pub struct Summary {
    pub text: String,
    pub words: u32,
}

#[derive(Contract, Debug)]
pub struct Watch {
    #[contract(min_len = 1)]
    pub viewer: String,
}

#[derive(Contract, Debug)]
pub struct Watching {
    pub notes: u32,
}

#[derive(Contract, Debug)]
pub struct Pin {
    pub id: u64,
    pub pinned: bool,
}

#[derive(Contract, Debug)]
#[contract(tag = "kind")]
pub enum NoteChange {
    Created {
        id: NoteId,
        title: String,
    },
    Pinned {
        id: NoteId,
        pinned: bool,
        by: String,
    },
    Deleted {
        id: NoteId,
    },
}

#[derive(Default, Debug)]
pub struct Store {
    notes: Mutex<BTreeMap<u64, Note>>,
}

impl Store {
    fn notes(&self) -> MutexGuard<'_, BTreeMap<u64, Note>> {
        self.notes.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn not_found(id: u64) -> Error {
    Error::not_found(format!("note {id} does not exist"))
}

fn notes_channel(store: &'static Store) -> Channel {
    Channel::new("notes:*")
        .doc("Live changes to a notebook. Join with who is watching.")
        .typed_join(
            "Who is watching.",
            move |socket: &Socket, w: Watch| -> Result<Watching, String> {
                socket.assign("viewer", Json::str(w.viewer));
                let notes = u32::try_from(store.notes().len()).unwrap_or(u32::MAX);
                Ok(Watching { notes })
            },
        )
        .typed_on(
            "pin",
            "Pin or unpin a note.",
            move |socket: &Socket, p: Pin| -> Result<Note, String> {
                let by = socket
                    .assign_get("viewer")
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default();
                let note = {
                    let mut notes = store.notes();
                    let note = notes
                        .get_mut(&p.id)
                        .ok_or_else(|| format!("note {} does not exist", p.id))?;
                    note.pinned = p.pinned;
                    note.clone()
                };
                socket.broadcast_typed(
                    "change",
                    &NoteChange::Pinned {
                        id: NoteId(p.id),
                        pinned: p.pinned,
                        by,
                    },
                );
                Ok(note)
            },
        )
        .typed_emits::<NoteChange>("change", "A note was created, pinned or deleted.")
}

pub fn app(store: &'static Store) -> App {
    let hub = Channels::new().channel(notes_channel(store)).build();
    let created = hub.clone();
    let deleted = hub.clone();
    App::new("notebook")
        .group("/api", vec![], move |g| {
            g.typed(
                Endpoint::get("notes.list", "/notes")
                    .doc("Notes, newest first, optionally by tag."),
                move |_c: &Ctx, q: ListNotes| -> Result<Page<Note>, Error> {
                    let notes = store.notes();
                    let matching: Vec<&Note> = notes
                        .values()
                        .rev()
                        .filter(|n| q.tag.as_ref().map_or(true, |t| n.tags.contains(t)))
                        .collect();
                    let limit = if q.limit == 0 { 20 } else { q.limit as usize };
                    Ok(Page {
                        total: u32::try_from(matching.len()).unwrap_or(u32::MAX),
                        items: matching.into_iter().take(limit).cloned().collect(),
                    })
                },
            )
            .typed(
                Endpoint::get("notes.get", "/notes/:id"),
                move |_c: &Ctx, r: NoteRef| -> Result<Note, Error> {
                    store
                        .notes()
                        .get(&r.id)
                        .cloned()
                        .ok_or_else(|| not_found(r.id))
                },
            )
            .typed(
                Endpoint::post("notes.create", "/notes").doc("Write a new note."),
                move |_c: &Ctx, n: NewNote| -> Result<Note, Error> {
                    let note = {
                        let mut notes = store.notes();
                        let id = notes.keys().next_back().map_or(1, |k| k + 1);
                        let note = Note {
                            id: NoteId(id),
                            title: n.title,
                            body: n.body,
                            tags: n.tags,
                            pinned: false,
                        };
                        notes.insert(id, note.clone());
                        note
                    };
                    created.broadcast_typed(
                        "notes:main",
                        "change",
                        &NoteChange::Created {
                            id: note.id.clone(),
                            title: note.title.clone(),
                        },
                    );
                    Ok(note)
                },
            )
            .typed(
                Endpoint::delete("notes.delete", "/notes/:id"),
                move |_c: &Ctx, r: NoteRef| -> Result<(), Error> {
                    store.notes().remove(&r.id).ok_or_else(|| not_found(r.id))?;
                    deleted.broadcast_typed(
                        "notes:main",
                        "change",
                        &NoteChange::Deleted { id: NoteId(r.id) },
                    );
                    Ok(())
                },
            )
            .typed_stream(
                Endpoint::post("notes.summarize", "/notes/:id/summarize")
                    .doc("Stream a summary, word by word."),
                move |_c: &Ctx, s: Summarize| -> Result<Stream<SummaryEvent, Summary>, Error> {
                    let note = store
                        .notes()
                        .get(&s.id)
                        .cloned()
                        .ok_or_else(|| not_found(s.id))?;
                    let cap = if s.words == 0 { 5 } else { s.words as usize };
                    Ok(Stream::new(move |out| {
                        let words: Vec<String> = format!("{} {}", note.title, note.body)
                            .split_whitespace()
                            .take(cap)
                            .map(str::to_string)
                            .collect();
                        let count = u32::try_from(words.len()).unwrap_or(u32::MAX);
                        out.emit(&SummaryEvent::Started { words: count })?;
                        for w in &words {
                            std::thread::sleep(Duration::from_millis(20));
                            out.emit(&SummaryEvent::Word { text: w.clone() })?;
                        }
                        Ok(Summary {
                            text: words.join(" "),
                            words: count,
                        })
                    }))
                },
            )
        })
        .channels("/live", "Live notebook changes.", hub)
        .get(
            "/channels.js",
            "The channels browser client.",
            |_c: &Ctx| {
                Response::new(200)
                    .with_header("content-type", "application/javascript")
                    .with_body(sutegi::channels::JS_CLIENT.as_bytes())
            },
        )
}
