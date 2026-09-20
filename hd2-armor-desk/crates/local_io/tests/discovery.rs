use local_io::{find_candidates, APP_ID, SAVE_NAME};

#[test]
fn finds_direct_save_below_a_custom_steam_root() {
    let temp = tempfile::tempdir().expect("create scratch directory");
    let steam_root = temp.path().join("games");
    let save = steam_root
        .join("userdata")
        .join("123456")
        .join(APP_ID)
        .join(SAVE_NAME);
    std::fs::create_dir_all(save.parent().expect("save has a parent"))
        .expect("create custom userdata layout");
    std::fs::write(&save, b"candidate").expect("write candidate save");

    let candidates = find_candidates(&steam_root);

    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].path, save);
    assert_eq!(candidates[0].account_dir, "123456");
    assert_eq!(candidates[0].size, 9);
}
