//! Live integration test: `DbStorage` over the Postgres backend. Runs only
//! when `SUTEGI_PG_TEST_URL` is set (same gate as the driver / queue / ORM
//! suites), so plain `cargo test` stays green without a server.

#![cfg(feature = "db")]

use sutegi_orm::pg::Pg;
use sutegi_orm::Backend;
use sutegi_storage::{DbStorage, Storage};

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
fn blob_roundtrip_on_postgres() {
    let Some(pg) = pg() else { return };
    let store = DbStorage::with_table(pg, "storage_it");
    store.migrate().expect("migrate");
    store
        .backend()
        .execute("DELETE FROM storage_it", &[])
        .unwrap();

    let blob: Vec<u8> = (0..=255u8).cycle().take(10_000).collect();
    store
        .put("it/blob.bin", &blob, "application/octet-stream")
        .unwrap();
    assert_eq!(store.get("it/blob.bin").unwrap().unwrap(), blob);

    let meta = store.stat("it/blob.bin").unwrap().unwrap();
    assert_eq!(meta.size, 10_000);
    assert_eq!(meta.content_type, "application/octet-stream");

    store.put("it/a.txt", b"a", "text/plain").unwrap();
    store.put("it/blob.bin", b"replaced", "text/plain").unwrap(); // upsert
    assert_eq!(store.get("it/blob.bin").unwrap().unwrap(), b"replaced");

    let keys: Vec<String> = store
        .list("it/")
        .unwrap()
        .into_iter()
        .map(|m| m.key)
        .collect();
    assert_eq!(keys, vec!["it/a.txt", "it/blob.bin"]);

    assert!(store.delete("it/a.txt").unwrap());
    assert!(store.delete("it/blob.bin").unwrap());
    assert_eq!(store.list("it/").unwrap().len(), 0);
}
