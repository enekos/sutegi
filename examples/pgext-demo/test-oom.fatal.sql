-- A Rust allocation that fails ends this session with FATAL: the script
-- passes when psql loses the connection (exit 2) and the server log shows no
-- crash (no backend killed by a signal, no reset of the other sessions).
CREATE EXTENSION pgext_demo;
SELECT alloc_bytes(4611686018427387903);
