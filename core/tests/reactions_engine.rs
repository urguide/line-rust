use anyhow::{Result, bail};
use line_rust_core::{
    store::Store,
    sync::{Command, Engine, Transport},
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicU64, mpsc},
    time::Duration,
};

struct Fake {
    events: mpsc::Sender<Value>,
}
impl Transport for Fake {
    fn call(&self, method: &str, p: Value, _: u64) -> Result<Value> {
        match method {
            "bridge_reactions" => Ok(
                json!({"at":100,"messages":[{"id":"9007199254740993","reactions":[
                    {"fromUserMid":"friend","reactionType":{"predefinedReactionType":"LOVE"},"atMillis":"50"}
                ]}]}),
            ),
            "bridge_react" if p["reaction"] == 7 => bail!("rejected"),
            "bridge_react" => Ok(json!({"sent":true})),
            _ => bail!("unexpected RPC {method}"),
        }
    }
    fn materialize(&self, row: &Value, _: u64) -> Result<Value> {
        Ok(row.clone())
    }
    fn emit(&self, event: Value) {
        self.events.send(event).unwrap();
    }
}

#[test]
fn others_react_to_my_message_and_failed_sends_do_not_replace_acknowledged_state() {
    let mut store = Store::open(Path::new(":memory:")).unwrap();
    store.set_account("me");
    store
        .save_messages(
            "upeer",
            &[json!({"id":"9007199254740993","mine":true,"from":"me","to":"upeer","text":"hello"})],
        )
        .unwrap();
    let (events, rx) = mpsc::channel();
    let engine = Engine::new(
        store,
        Fake { events },
        Arc::new(AtomicU64::new(1)),
        "/nonexistent/reactions".into(),
    );
    let (tx, commands) = mpsc::channel();
    let worker = std::thread::spawn(move || engine.run(commands));
    tx.send(Command::Session {
        account: "me".into(),
        generation: 1,
    })
    .unwrap();
    // Exercise the asynchronous snapshot completion used by the desktop UI.
    tx.send(Command::FocusChat {
        chat: "upeer".into(),
        generation: 1,
    })
    .unwrap();
    for (id, method, reaction) in [
        (1, "sync_reactions", 0),
        (2, "react", 2),
        (3, "react", 7),
        (4, "react", 1),
    ] {
        tx.send(Command::Operation {
            id,
            method: method.into(),
            params: json!({"chatMid":"upeer","messageId":"9007199254740993","reaction":reaction}),
            generation: 1,
        })
        .unwrap();
        let first = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        if reaction == 7 {
            assert_eq!(first["ok"], false);
            assert_eq!(first["error"], "rejected");
            continue;
        }
        assert_eq!(first["event"], "reactions");
        assert_eq!(first["account"], "me");
        assert_eq!(first["chatMid"], "upeer");
        let reactions = first["messages"][0]["reactions"].as_array().unwrap();
        assert!(
            reactions
                .iter()
                .any(|r| r["fromUserMid"] == "friend" && r["reaction"] == 3)
        );
        assert_eq!(reactions.len(), if reaction == 2 { 2 } else { 1 });
        let response = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(response["id"], id);
        assert_eq!(response["ok"], true);
    }
    tx.send(Command::Stop).unwrap();
    worker.join().unwrap();
}
