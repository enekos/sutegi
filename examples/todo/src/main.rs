//! A complete (in-memory) sutegi app touching every pillar:
//! routing + extractors, the ORM query builder + model introspection, and a
//! first-class AI tool. Run it, then:
//!
//! ```text
//! curl localhost:8080/__introspect       # full app surface as JSON
//! curl localhost:8080/__tools            # LLM tool manifest
//! curl -X POST localhost:8080/__tools/create_todo -d '{"title":"ship sutegi"}'
//! curl localhost:8080/todos
//! ```

use std::sync::Mutex;

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

/// A tiny in-memory store standing in for a database, so the demo is runnable
/// with zero setup. The ORM query builder still shows how real SQL is formed.
static STORE: Mutex<Vec<(i64, String, bool)>> = Mutex::new(Vec::new());

fn todos_json() -> Json {
    let store = STORE.lock().unwrap();
    Json::arr(
        store
            .iter()
            .map(|(id, title, done)| {
                Json::obj(vec![
                    ("id", Json::int(*id)),
                    ("title", Json::str(title.clone())),
                    ("done", Json::Bool(*done)),
                ])
            })
            .collect(),
    )
}

/// An AI tool: create a todo. This is what an agent calls via
/// `POST /__tools/create_todo`.
struct CreateTodo;

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
        let mut store = STORE.lock().unwrap();
        let id = store.len() as i64 + 1;
        store.push((id, title.clone(), false));
        Ok(Json::obj(vec![
            ("id", Json::int(id)),
            ("title", Json::str(title)),
            ("done", Json::Bool(false)),
        ]))
    }
}

fn main() -> std::io::Result<()> {
    let app = App::new("todo-demo")
        .register_model(sutegi::orm::schema_json(&Todo::schema()))
        .get("/", "Health check.", |_req, _p| text(200, "sutegi up"))
        .get("/todos", "List all todos.", |_req, _p| json(200, &todos_json()))
        .get("/todos/:id", "Show the SQL the ORM builds for one todo.", |_req, p| {
            let id = p.get("id").cloned().unwrap_or_default();
            // Demonstrate the parameterized query builder.
            let (sql, params) = Todo::query()
                .select(&["id", "title", "done"])
                .filter("id", "=", Value::Text(id.clone()))
                .limit(1)
                .build();
            json(
                200,
                &Json::obj(vec![
                    ("id", Json::str(id)),
                    ("sql", Json::str(sql)),
                    ("params", Json::arr(params.iter().map(|v| v.to_json()).collect())),
                ]),
            )
        });

    // Mount the AI tool surface: /__tools and /__tools/:name.
    let app = sutegi::ai::mount(app, ToolRegistry::new().add(CreateTodo));

    // Bind address may be overridden as the first CLI argument.
    let addr = std::env::args().nth(1).unwrap_or_else(|| "127.0.0.1:8080".to_string());
    println!("sutegi todo-demo on http://{addr}");
    println!("  GET  /__introspect   — full app surface");
    println!("  GET  /__tools        — LLM tool manifest");
    println!("  POST /__tools/create_todo  -d '{{\"title\":\"...\"}}'");
    app.run(&addr)
}
