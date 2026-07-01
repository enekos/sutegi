//! A complete sutegi app in ~60 lines — every pillar, almost no ceremony:
//!
//! * `#[derive(Model, Validate)]` — typed model + its own validation rules
//! * pooled SQLite state via `.state(db)` (no `Arc<Mutex<…>>`)
//! * handlers take one `&Ctx`, return `impl IntoResponse`, and use `?`
//! * route **groups** + group **middleware**, **route-model binding** (`c.model`)
//! * a first-class **AI tool** and a **streaming** tool, sharing the same state
//! * versioned migrations with a `todo migrate[:status|:rollback]` CLI
//!
//! ```text
//! curl localhost:8080/__introspect
//! curl -X POST localhost:8080/api/todos -d '{"title":"ship sutegi"}'
//! curl localhost:8080/api/todos
//! curl localhost:8080/api/todos/1
//! curl -X POST localhost:8080/__tools/create_todo -d '{"title":"via agent"}'
//! ```

use std::time::Duration;

use sutegi::prelude::*;

/// The `todos` table. Schema, hydration, JSON, `save()`, and the validation
/// ruleset are all derived — the struct is the single source of truth.
#[derive(Model, Validate)]
#[model(table = "todos")]
struct Todo {
    #[model(primary)]
    id: i64,
    #[validate(required, str, min_len = 1, max_len = 200)]
    title: String,
    done: bool,
}

/// Versioned migrations, applied on startup and via the `todo migrate` CLI.
fn migrations() -> Migrator {
    Migrator::new().add(Migration::reversible(
        "20260701_000001",
        "create_todos",
        |db| db.migrate_schema(&Todo::schema()),
        |db| db.execute("DROP TABLE todos", &[]).map(|_| ()),
    ))
}

fn main() -> std::io::Result<()> {
    // Pooled, Send+Sync SQLite — persisted when DATABASE_PATH is set, else
    // in-memory. `todo migrate[:status|:rollback]` runs and exits before serving.
    let db = Db::open_or_memory("DATABASE_PATH");
    if sutegi::migrate::dispatch(&migrations(), &db) {
        return Ok(());
    }
    migrations().run(&db).expect("migrate");

    let ready = db.clone(); // readiness probe holds its own handle
    App::new("todo")
        .state(db)
        .readiness(move || ready.query("SELECT 1", &[]).is_ok())
        .get("/", "Health check.", |_| "sutegi up")
        .get(
            "/stream",
            "SSE demo: three ticks then a done event.",
            |_| {
                sse(|sink| {
                    for i in 1..=3 {
                        sink.data(&format!("tick {i}"))?;
                        std::thread::sleep(Duration::from_millis(80));
                    }
                    sink.event("done", "bye")
                })
            },
        )
        .group("/api", vec![mw(log_request)], |g| {
            g.get("/todos", "List all todos.", |c| -> Result<Json, Error> {
                let todos = Todo::all_typed(c.db::<Db>())?;
                Ok(Json::arr(todos.iter().map(Todo::to_json).collect()))
            })
            .get("/todos/:id", "Fetch one todo (route-model binding).", |c| {
                c.model::<Todo, Db>("id").map(|t| t.to_json())
            })
            .post("/todos", "Create a todo (validated).", |c| {
                let todo: Todo = c.validated()?;
                let id = todo.save(c.db::<Db>())?;
                Ok::<_, Error>((201, Todo { id, ..todo }.to_json()))
            })
        })
        .tool(
            "create_todo",
            "Create a new todo item with the given title.",
            schema::object(
                vec![("title", schema::string("the todo's title"))],
                &["title"],
            ),
            |c, args| {
                let todo = Todo::from_input(&args)?; // lenient: no id/done needed
                let id = todo.save(c.db::<Db>())?;
                Ok(Todo { id, ..todo }.to_json())
            },
        )
        .stream_tool(
            "stream_answer",
            "Stream an answer token-by-token as Server-Sent Events.",
            schema::object(vec![("prompt", schema::string("the prompt"))], &["prompt"]),
            |_c, args, sink| {
                let prompt = args.get("prompt").and_then(Json::as_str).unwrap_or("");
                let reply = format!("you asked: {prompt} — here is your streamed reply.");
                for token in reply.split(' ') {
                    sink.data(token)?;
                    std::thread::sleep(Duration::from_millis(60));
                }
                sink.event("done", "{}")
            },
        )
        .serve()
}

/// Group middleware: log every request to the `/api` group, then continue.
fn log_request(req: &Request) -> Option<Response> {
    println!("[mw] {} {}", req.method.as_str(), req.path);
    None
}
