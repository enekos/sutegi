//! Live integration test: the user system over the Postgres backend. Runs
//! only when `SUTEGI_PG_TEST_URL` is set (same gate as the other suites).

use sutegi_auth::{Tokens, Users};
use sutegi_orm::pg::Pg;
use sutegi_orm::Backend;

fn pg() -> Option<Pg> {
    let url = match std::env::var("SUTEGI_PG_TEST_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("skipping: SUTEGI_PG_TEST_URL not set");
            return None;
        }
    };
    Some(Pg::connect(&url, 4).expect("connect to test postgres"))
}

#[test]
fn users_and_tokens_on_postgres() {
    let Some(pg) = pg() else { return };
    let users = Users::new(pg.clone()).iterations(1_000);
    users.migrate().expect("migrate users");
    users.backend().execute("DELETE FROM users", &[]).unwrap();
    let tokens = Tokens::new(pg);
    tokens.migrate().expect("migrate tokens");

    let u = users
        .register_with("pg-it@example.com", "password1", "IT", "admin")
        .unwrap();
    assert!(u.id > 0);
    assert!(users.register("pg-it@example.com", "password2").is_err()); // unique

    assert_eq!(
        users
            .authenticate("PG-IT@example.com", "password1")
            .unwrap()
            .unwrap()
            .id,
        u.id
    );
    assert!(users
        .authenticate("pg-it@example.com", "wrong password")
        .unwrap()
        .is_none());

    let (plain, rec) = tokens.issue(u.id, "agent").unwrap();
    assert_eq!(tokens.verify(&plain).unwrap(), Some(u.id));
    assert!(tokens.revoke(rec.id).unwrap());
    assert_eq!(tokens.verify(&plain).unwrap(), None);

    users.set_password(u.id, "rotated-pass").unwrap();
    assert!(users
        .authenticate("pg-it@example.com", "rotated-pass")
        .unwrap()
        .is_some());
    assert!(users.delete(u.id).unwrap());
}
