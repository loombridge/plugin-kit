//! Example Modular's parameter IDs never change: `tests/ids.txt` keeps
//! every ID it has published with what it means, and may only be added to.

#[test]
fn published_ids_keep_their_meaning() {
    lspk_test::ids::assert_ids(
        &lspk_example_modular::space().id_list(),
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/ids.txt"),
    );
}
