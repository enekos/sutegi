//! A complete sutegi app exercising every pillar and the newest features:
//!
//! * `#[derive(Model)]` — typed model, schema, JSON hydration (bools stay bools)
//! * SQLite ORM — `migrate` / `all_typed` / `find_typed` / `create`
//! * route **groups** + group **middleware** (`/api`, request logger)
//! * **route-model binding** — `GET /api/todos/:id` hydrates a `Todo` or 404s
//! * a background **job queue** — creating a todo dispatches a notify job
//! * Laravel-style **validation** + a first-class **AI tool** sharing the DB
//!
//! ```text
//! curl localhost:8080/__introspect
//! curl -X POST localhost:8080/api/todos -d '{"title":"ship sutegi"}'
//! curl localhost:8080/api/todos
//! curl localhost:8080/api/todos/1
//! curl localhost:8080/__queue
//! curl -X POST localhost:8080/__tools/create_todo -d '{"title":"via agent"}'
//! ```

use std::sync::{Arc, Mutex};

use sutegi::binding;
use sutegi::prelude::*;

/// The `todos` table — schema, hydration, and serialization are all derived.
#[derive(Model)]
#[model(table = "todos")]
struct Todo {
    #[model(primary)]
    id: i64,
    title: String,
    done: bool,
}

type Database = Arc<Mutex<Db>>;

/// Validation rules for creating a todo — Laravel `Validator` style.
fn create_todo_rules() -> Ruleset {
    Ruleset::new()
        .field("title", &[Rule::Required, Rule::Str, Rule::MinLen(1), Rule::MaxLen(200)])
        .field("done", &[Rule::Bool])
}

/// A background job dispatched after a todo is created.
struct NotifyJob {
    title: String,
}

impl Job for NotifyJob {
    fn name(&self) -> &str {
        "notify"
    }
    fn handle(&self) -> Result<(), String> {
        println!("[job:notify] a todo was created: {:?}", self.title);
        Ok(())
    }
}

/// An AI tool that creates a todo (same DB + queue as the HTTP routes).
struct CreateTodo {
    db: Database,
    queue: Arc<Queue>,
}

impl Tool for CreateTodo {
    fn name(&self) -> &str {
        "create_todo"
    }
    fn description(&self) -> &str {
        "Create a new todo item with the given title."
    }
    fn parameters(&self) -> Json {
        schema::object(vec![("title", schema::string("the todo's title"))], &["title"])
    }
    fn call(&self, args: Json) -> Result<Json, String> {
        let title = args
            .get("title")
            .and_then(|j| j.as_str())
            .ok_or("title must be a string")?
            .to_string();
        let id = {
            let db = self.db.lock().unwrap();
            Todo::create(&db, &[("title", Value::Text(title.clone())), ("done", Value::Bool(false))])?
        };
        self.queue.dispatch(NotifyJob { title: title.clone() });
        Ok(Todo { id, title, done: false }.to_json())
    }
}

fn main() -> std::io::Result<()> {
    let db = Db::memory().expect("open db");
    Todo::migrate(&db).expect("migrate");
    let db: Database = Arc::new(Mutex::new(db));
    let queue = Queue::new(2);

    // Per-route clones for the handlers / group / tool.
    let list_db = Arc::clone(&db);
    let show_db = Arc::clone(&db);
    let create_db = Arc::clone(&db);
    let create_queue = Arc::clone(&queue);
    let stats_queue = Arc::clone(&queue);

    // A group middleware: log every request to the /api group.
    let logger = mw(|req: &Request| {
        println!("[mw] {} {}", req.method.as_str(), req.path);
        None // continue to the handler
    });

    let app = App::new("todo-demo")
        .register_model(sutegi::orm::schema_json(&Todo::schema()))
        .get("/", "Health check.", |_req, _p| text(200, "sutegi up"))
        .get("/__queue", "Background queue stats.", move |_req, _p| {
            json(200, &stats_queue.stats().to_json())
        })
        .group("/api", vec![logger], move |g| {
            let list_db = Arc::clone(&list_db);
            let show_db = Arc::clone(&show_db);
            let create_db = Arc::clone(&create_db);
            let create_queue = Arc::clone(&create_queue);
            g.get("/todos", "List all todos.", move |_req, _p| {
                let db = list_db.lock().unwrap();
                match Todo::all_typed(&db) {
                    Ok(todos) => json(200, &Json::arr(todos.iter().map(Todo::to_json).collect())),
                    Err(e) => json(500, &Json::obj(vec![("error", Json::str(e))])),
                }
            })
            .get("/todos/:id", "Fetch one todo (route-model binding).", move |_req, p| {
                let db = show_db.lock().unwrap();
                match binding::model::<Todo>(&db, p, "id") {
                    Ok(todo) => json(200, &todo.to_json()),
                    Err(resp) => resp, // 404/500 already built
                }
            })
            .post("/todos", "Create a todo (validated).", move |req, _p| {
                let body = match json_body(req) {
                    Ok(b) => b,
                    Err(e) => return json(400, &Json::obj(vec![("error", Json::str(e))])),
                };
                if let Err(errs) = create_todo_rules().validate(&body) {
                    return json(
                        422,
                        &Json::obj(vec![
                            ("error", Json::str("validation failed")),
                            ("errors", errs.to_json()),
                        ]),
                    );
                }
                let title = body.get("title").and_then(|j| j.as_str()).unwrap_or("").to_string();
                let done = body.get("done").and_then(|j| j.as_bool()).unwrap_or(false);
                let id = {
                    let db = create_db.lock().unwrap();
                    match Todo::create(&db, &[("title", Value::Text(title.clone())), ("done", Value::Bool(done))]) {
                        Ok(id) => id,
                        Err(e) => return json(500, &Json::obj(vec![("error", Json::str(e))])),
                    }
                };
                create_queue.dispatch(NotifyJob { title: title.clone() });
                json(201, &Todo { id, title, done }.to_json())
            })
        });

    let app = sutegi::ai::mount(
        app,
        ToolRegistry::new().add(CreateTodo {
            db: Arc::clone(&db),
            queue: Arc::clone(&queue),
        }),
    );

    let addr = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:8080".to_string());
    println!("sutegi todo-demo on http://{addr}");
    println!("  GET  /__introspect | /__tools | /__queue");
    println!("  /api/todos (GET, POST), /api/todos/:id (GET, route-model binding)");
    app.run(&addr)
}
