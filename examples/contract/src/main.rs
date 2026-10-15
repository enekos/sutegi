fn main() -> std::io::Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:4790".to_string());
    let store: &'static contract_example::Store = Box::leak(Box::default());
    println!("notebook on http://{addr} — GET /__contract.ts for the client");
    contract_example::app(store).run(&addr)
}
