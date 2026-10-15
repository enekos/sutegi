#[test]
fn checked_in_client_matches_the_contract() {
    let store: &'static contract_example::Store = Box::leak(Box::default());
    let ts = contract_example::app(store).contract_ts();
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/client/api.ts");
    if let Err(e) = sutegi::contract::snapshot::check_file(path, &ts) {
        panic!("{e}");
    }
}
