//! Composition root — the ONE place that knows about concrete adapters. It
//! picks an outbound adapter, injects it into the use cases, and mounts the
//! inbound adapters. Everything else depends only on traits.
//!
//! ```text
//! cargo run -p hexagonal-example                 # SQLite adapter (default)
//! REPO=memory cargo run -p hexagonal-example     # in-memory adapter
//!
//! curl localhost:8080/api/todos
//! curl -X POST localhost:8080/api/todos -d '{"title":"ship sutegi"}'
//! curl -X POST localhost:8080/api/todos/1/complete
//! curl -X POST localhost:8080/__tools/create_todo -d '{"title":"via agent"}'
//! ```

mod adapters;
mod application;
mod domain;
mod ports;

use std::sync::{Arc, Mutex};

use sutegi::prelude::*;

use adapters::inbound_ai::CreateTodoTool;
use adapters::inbound_http;
use adapters::outbound_memory::InMemoryTodoRepo;
use adapters::outbound_sqlite::SqliteTodoRepo;
use application::{CompleteTodo, CreateTodo, ListTodos};
use ports::TodoRepository;

fn main() -> std::io::Result<()> {
    // 1. Choose the outbound adapter (REPO=memory|sqlite). This is the only
    //    code aware of a concrete implementation.
    let repo: Arc<dyn TodoRepository> = match sutegi::env_or("REPO", "sqlite").as_str() {
        "memory" => Arc::new(InMemoryTodoRepo::new()),
        _ => {
            let db = Arc::new(Mutex::new(Db::memory().expect("open db")));
            Arc::new(SqliteTodoRepo::new(db).expect("init sqlite repo"))
        }
    };

    // 2. Wire the use cases (inbound ports) over the chosen repo.
    let create = Arc::new(CreateTodo { repo: Arc::clone(&repo) });
    let list = Arc::new(ListTodos { repo: Arc::clone(&repo) });
    let complete = Arc::new(CompleteTodo { repo: Arc::clone(&repo) });

    // 3. Mount the inbound adapters — HTTP and AI both over the same use cases.
    let app = App::new("hexagonal-todo")
        .readiness(|| true)
        .get("/", "Health check.", |_req, _p| text(200, "sutegi up"));
    let app = inbound_http::register(app, Arc::clone(&create), list, complete);
    let app = sutegi::ai::mount(
        app,
        ToolRegistry::new().add(CreateTodoTool { use_case: create }),
    );

    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| format!("{}:{}", sutegi::env_or("HOST", "0.0.0.0"), sutegi::env_or("PORT", "8080")));
    println!("hexagonal-todo on http://{addr}  (REPO={})", sutegi::env_or("REPO", "sqlite"));
    app.run_graceful(&addr)
}
