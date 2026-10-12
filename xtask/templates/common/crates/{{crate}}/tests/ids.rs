//! {{Name}}'s parameter IDs never change: `tests/ids.txt` keeps every ID it
//! has published with what it means, and may only be added to. When you add
//! a parameter, run this test with `LSPK_IDS_UPDATE=1` and commit the list.

#[test]
fn published_ids_keep_their_meaning() {
    lspk_test::ids::assert_ids(
        &{{crate_lib}}::space().id_list(),
        concat!(env!("CARGO_MANIFEST_DIR"), "/tests/ids.txt"),
    );
}
