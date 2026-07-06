//! The bridge's core guarantee, tested without a reactor: a session is its
//! journal. Feed a program a mixed input stream (dispatches, effect
//! completions, sub ticks) while journaling; replay the journal into a
//! fresh program; both must rerender to identical wire bytes.

use super::*;
use zumar_core::{el, text, VNode};
use zumar_runtime::effects::{delay, every_with_now, http_get, Cmds, HttpResult, Sub};

#[derive(Clone)]
enum TMsg {
    Inc,
    Lazy,
    Bump,
    Fetch,
    Got(HttpResult),
    Toggle,
    Tick(f64),
}

struct TModel {
    count: i64,
    running: bool,
    last: String,
    ticks: u32,
}

fn update(model: &mut TModel, msg: TMsg) -> Cmds<TMsg> {
    match msg {
        TMsg::Inc => model.count += 1,
        TMsg::Lazy => return vec![delay(5, TMsg::Bump)],
        TMsg::Bump => model.count += 10,
        TMsg::Fetch => return vec![http_get("/api/x", TMsg::Got)],
        TMsg::Got(r) => model.last = r.body,
        TMsg::Toggle => model.running = !model.running,
        TMsg::Tick(now) => {
            model.ticks += 1;
            model.count += (now as i64) % 7;
        }
    }
    Vec::new()
}

fn view(model: &TModel) -> VNode<TMsg> {
    el("div")
        .child(el("button").on("click", TMsg::Inc).child(text("+")))
        .child(el("button").on("click", TMsg::Lazy).child(text("lazy")))
        .child(el("button").on("click", TMsg::Fetch).child(text("fetch")))
        .child(el("button").on("click", TMsg::Toggle).child(text("toggle")))
        .child(text(format!(
            "{}|{}|{}",
            model.count, model.last, model.ticks
        )))
        .into()
}

fn subs(model: &TModel) -> Vec<Sub<TMsg>> {
    if model.running {
        vec![every_with_now(100, TMsg::Tick)]
    } else {
        Vec::new()
    }
}

fn program() -> Program<TModel, TMsg> {
    Program::new(
        TModel {
            count: 0,
            running: false,
            last: String::new(),
            ticks: 0,
        },
        update,
        view,
    )
    .with_subscriptions(subs)
}

fn click(path: &[u32]) -> Frame {
    Frame::Dispatch {
        path: path.to_vec(),
        name: "click".into(),
        value: None,
        checked: None,
        key: None,
    }
}

#[test]
fn replay_reproduces_the_exact_session() {
    let journal = MemJournal::default();
    let mut live = program();
    live.initial_render();

    // A realistic mixed session. Effect ids come from the live run: Lazy's
    // delay is the first fresh id after init (1), Fetch's is next, the sub
    // start after Toggle the one after — mirror what the bridge journals.
    let mut inputs: Vec<Frame> = vec![click(&[0]), click(&[0]), click(&[1])];
    // apply the three dispatches, capturing the delay cmd id
    let mut delay_id = 0;
    for f in &inputs {
        let up = apply(&mut live, f);
        if let Some(cmd) = up.cmds.first() {
            delay_id = cmd.id;
        }
    }
    // the delay completes
    let done = Frame::Resolve {
        id: delay_id,
        ok: true,
        status: 0,
        body: String::new(),
    };
    apply(&mut live, &done);
    inputs.push(done);
    // fetch + completion
    let fetch = click(&[2]);
    let up = apply(&mut live, &fetch);
    inputs.push(fetch);
    let got = Frame::Resolve {
        id: up.cmds[0].id,
        ok: true,
        status: 200,
        body: "aupa".into(),
    };
    apply(&mut live, &got);
    inputs.push(got);
    // toggle the ticker on, let it fire twice with server timestamps
    let toggle = click(&[3]);
    let up = apply(&mut live, &toggle);
    inputs.push(toggle);
    let SubDelta::Start { id: sub_id, .. } = up.subs[0] else {
        panic!("expected sub start");
    };
    for now in [1_720_000_000_123u64, 1_720_000_000_224] {
        let tick = Frame::Notify { id: sub_id, now };
        apply(&mut live, &tick);
        inputs.push(tick);
    }

    for f in &inputs {
        journal.append("s", &frames::encode(f)).unwrap();
    }

    // reconnect: fresh program fast-forwarded through the journal
    let mut replayed = program();
    replayed.initial_render();
    for bytes in journal.load("s").unwrap() {
        apply(&mut replayed, &frames::decode(&bytes).unwrap());
    }

    let a = live.rerender();
    let b = replayed.rerender();
    assert_eq!(a.to_bytes(), b.to_bytes());
    // and the state visibly progressed (2 incs + lazy bump + ticks)
    let tree = format!("{:?}", a.root);
    assert!(tree.contains("aupa"), "{tree}");
    assert!(tree.contains("|2"), "ticks should be 2: {tree}");
    // active subs re-arm on reconnect: the Start delta is in the rerender
    assert_eq!(a.subs.len(), 1);
    assert_eq!(b.subs.len(), 1);
}

#[test]
fn journal_frames_round_trip_through_the_codec() {
    // the bridge journals its own encodings; every frame it can produce
    // must decode back to itself (server-side completions included)
    for f in [
        click(&[3]),
        Frame::Resolve {
            id: 9,
            ok: false,
            status: 502,
            body: "error 502".into(),
        },
        Frame::Notify {
            id: 4,
            now: u64::MAX / 2,
        },
    ] {
        assert_eq!(frames::decode(&frames::encode(&f)).unwrap(), f);
    }
}
