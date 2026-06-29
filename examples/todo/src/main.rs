//! A complete, runnable sutegi app touching every pillar:
//! routing + extractors, a **real SQLite-backed ORM** (migrate / Eloquent-style
//! `all` / `find` / `create`), Laravel-style **request validation**, and a
//! first-class **AI tool** that shares the same database.
//!
//! ```text
//! curl localhost:8080/__introspect
//! curl localhost:8080/todos
//! curl -X POST localhost:8080/todos -d '{"title":"ship sutegi"}'
//! curl -X POST localhost:8080/todos -d '{"title":""}'          # validation error
//! curl -X POST localhost:8080/__tools/create_todo -d '{"title":"via agent"}'
//! curl localhost:8080/todos/1
//! ```

use std::sync::{Arc, Mutex};

use sutegi::prelude::*;

/// The `todos` table.
struct Todo;

impl Model for Todo {
    fn schema() -> TableSchema {
        TableSchema {
            table: "todos",
            columns: vec![
                Column { name: "id", ty: ColType::Integer, nullable: false, primary: true },
                Column { name: "title", ty: ColType::Text, nullable: false, primary: false },
                Column { name: "done", ty: ColType::Boolean, nullable: false, primary: false },
            ],
        }
    }
}

type Database = Arc<Mutex<Db>>;

/// Validation rules for creating a todo — Laravel `Validator` style.
fn create_todo_rules() -> Ruleset {
    Ruleset::new()
        .field("title", &[Rule::Required, Rule::Str, Rule::MinLen(1), Rule::MaxLen(200)])
        .field("done", &[Rule::Bool])
}

/// An AI tool that creates a todo in the same database the HTTP routes use.
struct CreateTodo {
    db: Database,
}

impl Tool for CreateTodo {
    fn name(&self) -> &str {
        "create_todo"
    }
    fn description(&self) -> &str {
        "Create a new todo item with the given title."
    }
    fn parameters(&self) -> Json {
        schema::object(
            vec![("title", schema::string("the todo's title"))],
            &["title"],
        )
    }
    fn call(&self, args: Json) -> Result<Json, String> {
        let title = args
            .get("title")
            .and_then(|j| j.as_str())
            .ok_or("title must be a string")?
            .to_string();
        let db = self.db.lock().unwrap();
        let id = Todo::create(
            &db,
            &[("title", Value::Text(title.clone())), ("done", Value::Bool(false))],
        )?;
        Ok(Json::obj(vec![
            ("id", Json::int(id)),
            ("title", Json::str(title)),
            ("done", Json::Bool(false)),
        ]))
    }
}

fn main() -> std::io::Result<()> {
    // Open an in-memory DB and run the migration from the model schema.
    let db = Db::memory().expect("open db");
    Todo::migrate(&db).expect("migrate");
    let db: Database = Arc::new(Mutex::new(db));

    let list_db = Arc::clone(&db);
    let show_db = Arc::clone(&db);
    let create_db = Arc::clone(&db);

    let app = App::new("todo-demo")
        .register_model(sutegi::orm::schema_json(&Todo::schema()))
        .get("/", "Health check.", |_req, _p| text(200, "sutegi up"))
        .get("/todos", "List all todos.", move |_req, _p| {
            let db = list_db.lock().unwrap();
            match Todo::all(&db) {
                Ok(rows) => json(200, &Json::arr(rows)),
                Err(e) => json(500, &Json::obj(vec![("error", Json::str(e))])),
            }
        })
        .get("/todos/:id", "Fetch one todo by id.", move |_req, p| {
            let id = p.get("id").cloned().unwrap_or_default();
            let db = show_db.lock().unwrap();
            match Todo::find(&db, Value::Text(id)) {
                Ok(Some(row)) => json(200, &row),
                Ok(None) => json(404, &Json::obj(vec![("error", Json::str("not found"))])),
                Err(e) => json(500, &Json::obj(vec![("error", Json::str(e))])),
            }
        })
        .post("/todos", "Create a todo (validated).", move |req, _p| {
            let body = match json_body(req) {
                Ok(b) => b,
                Err(e) => return json(400, &Json::obj(vec![("error", Json::str(e))])),
            };
            // Laravel-style validation; structured 422 on failure.
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
            let db = create_db.lock().unwrap();
            match Todo::create(&db, &[("title", Value::Text(title.clone())), ("done", Value::Bool(done))]) {
                Ok(id) => json(
                    201,
                    &Json::obj(vec![
                        ("id", Json::int(id)),
                        ("title", Json::str(title)),
                        ("done", Json::Bool(done)),
                    ]),
                ),
                Err(e) => json(500, &Json::obj(vec![("error", Json::str(e))])),
            }
        });

    // Mount the AI tool surface (/__tools, /__tools/:name), sharing the DB.
    let app = sutegi::ai::mount(app, ToolRegistry::new().add(CreateTodo { db: Arc::clone(&db) }));

    let addr = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:8080".to_string());
    println!("sutegi todo-demo on http://{addr}");
    println!("  GET  /__introspect          — full app surface");
    println!("  GET  /__tools               — LLM tool manifest");
    println!("  POST /todos -d '{{\"title\":\"...\"}}'   — validated create");
    app.run(&addr)
}
