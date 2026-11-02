use line_rust_core::{
    message, reactions,
    store::{Store, merge},
};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};

#[test]
fn reactions_deduplicate_users_handle_changes_undo_and_lossless_history() {
    let raw = json!({"id":"9007199254740993","from":"me","to":"upeer","reactions":[
        {"fromUserMid":"u1","atMillis":"10","reactionType":{"predefinedReactionType":"NICE"}},
        {"fromUserMid":"u1","atMillis":"20","reactionType":{"predefinedReactionType":"LOVE"}},
        {"fromUserMid":"u2","atMillis":30,"reactionType":{"predefinedReactionType":4}},
        {"fromUserMid":"u2","atMillis":40,"reactionType":{"predefinedReactionType":"UNDO"}},
        {"1":"u3","2":"50","3":{"1":7}},
        {"fromUserMid":"u4","reaction":99}
    ]});
    let row = message::from_envelope(
        &json!({"rawMessage":raw,"decodedMessage":raw,"reactionsUpdatedAt":100}),
        "me",
    );
    assert_eq!(row["id"], "9007199254740993");
    assert_eq!(row["mine"], true);
    assert_eq!(
        row["reactions"],
        json!([
            {"fromUserMid":"u1","atMillis":20,"reaction":3},
            {"fromUserMid":"u3","atMillis":50,"reaction":7}
        ])
    );
    for (name, n) in [
        ("NICE", 2),
        ("LOVE", 3),
        ("FUN", 4),
        ("AMAZING", 5),
        ("SAD", 6),
        ("OMG", 7),
    ] {
        assert_eq!(reactions::kind(&json!(name)), Some(n));
    }
    let stale = merge(
        row.clone(),
        &json!({"reactions":[],"reactionsUpdatedAt":99}),
    );
    assert_eq!(stale["reactions"], row["reactions"]);
    let missing = merge(
        row.clone(),
        &json!({"reactions":null,"reactionsUpdatedAt":200}),
    );
    assert_eq!(missing["reactions"], row["reactions"]);
    assert_eq!(missing["reactionsUpdatedAt"], 100);
    let cleared = merge(row, &json!({"reactions":[],"reactionsUpdatedAt":101}));
    assert_eq!(cleared["reactions"], json!([]));
}

#[test]
fn snapshots_persist_without_touching_age_or_resurrecting_messages_or_other_accounts() {
    let root = std::env::temp_dir().join(format!("line-reactions-{}.sqlite", std::process::id()));
    let clock = Arc::new(AtomicI64::new(1000));
    let now = clock.clone();
    let mut store =
        Store::open_with_clock(&root, Arc::new(move || now.load(Ordering::SeqCst))).unwrap();
    for account in ["me", "other"] {
        store.set_account(account);
        store
            .save_messages(
                "cgroup",
                &[json!({"id":"9007199254740993","text":"original","createdTime":500,"mine":true})],
            )
            .unwrap();
    }
    store.set_account("me");
    let snapshot = json!([{"id":"9007199254740993","reactions":[{"fromUserMid":"friend","reaction":2,"atMillis":10}]}]);
    store.update_reactions("cgroup", &snapshot, 100).unwrap();
    let db = rusqlite::Connection::open(&root).unwrap();
    let age: i64 = db
        .query_row("SELECT updated FROM messages WHERE account='me'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(age, 1000);
    drop(db);
    assert!(
        store
            .update_reactions(
                "cgroup",
                &json!([{"id":"9007199254740993","reactions":[]}]),
                99
            )
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .update_reactions("wrong", &snapshot, 101)
            .unwrap()
            .is_empty()
    );
    drop(store);
    let mut store = Store::open(&root).unwrap();
    store.set_account("me");
    assert_eq!(
        store
            .message("cgroup", "9007199254740993")
            .unwrap()
            .unwrap()["reactions"][0]["reaction"],
        2
    );
    store.set_account("other");
    assert!(
        store
            .message("cgroup", "9007199254740993")
            .unwrap()
            .unwrap()["reactions"]
            .is_null()
    );
    store.set_account("me");
    store.unsend("cgroup", "9007199254740993").unwrap();
    assert!(
        store
            .update_reactions("cgroup", &snapshot, 200)
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .message("cgroup", "9007199254740993")
            .unwrap()
            .is_none()
    );
    let db = rusqlite::Connection::open(&root).unwrap();
    let age: i64 = db
        .query_row(
            "SELECT updated FROM messages WHERE account='other'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(age, 1000);
    drop(db);
    drop(store);
    std::fs::remove_file(root).unwrap();
}
