use anyhow::{Result, bail};
use line_rust_core::{
    store::{Cursor, Page, Store},
    sync::{Command, Engine, Transport},
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicU64, mpsc},
    time::Duration,
};

fn page(head: u64) -> Page {
    Page {
        messages: (head-19..=head).map(|n| json!({"id":n.to_string(),"createdTime":n,"contentType":"NONE","text":format!("message {n}")})).collect(),
        next_cursor: Some(Cursor { message_id:(head-19).to_string(), delivered_time:(head-19).to_string() }),
        has_more:true, cached:false,
    }
}

struct Mock {
    events: mpsc::Sender<Value>,
    calls: mpsc::Sender<String>,
}
impl Transport for Mock {
    fn call(&self, method: &str, params: Value, _: u64) -> Result<Value> {
        self.calls.send(method.into())?;
        match method {
            "list_chats" => Ok(
                json!({"chats":[{"mid":"chat","lastActivity":160,"lastMessage":{"messageId":"160","deliveredTime":"160"}}]}),
            ),
            "bridge_chat_preview" => {
                assert_eq!(params["cursor"]["messageId"], "160");
                Ok(
                    json!({"messages":[{"id":"160","createdTime":160,"text":"latest","contentType":"NONE"}]}),
                )
            }
            "bridge_fetch_page" => {
                let head = params["before"]["messageId"]
                    .as_str()
                    .map(|s| s.parse::<u64>().unwrap() - 1)
                    .unwrap_or(160);
                Ok(serde_json::to_value(page(head))?)
            }
            _ => bail!("unexpected {method}"),
        }
    }
    fn materialize(&self, _: &Value, _: u64) -> Result<Value> {
        panic!("sidebar must not download media")
    }
    fn emit(&self, event: Value) {
        let _ = self.events.send(event);
    }
}

#[test]
fn preview_repair_does_not_skip_missing_history_when_chat_opens() {
    let mut store = Store::open(Path::new(":memory:")).unwrap();
    store.set_account("self");
    store.save_page("chat", None, &page(100)).unwrap();
    let (events, rx) = mpsc::channel();
    let (calls, work) = mpsc::channel();
    let engine = Engine::new(
        store,
        Mock { events, calls },
        Arc::new(AtomicU64::new(1)),
        "/nonexistent".into(),
    );
    let (tx, commands) = mpsc::channel();
    tx.send(Command::Session {
        account: "self".into(),
        generation: 1,
    })
    .unwrap();
    tx.send(Command::Operation {
        id: 1,
        method: "list_chats".into(),
        params: json!({}),
        generation: 1,
    })
    .unwrap();
    let worker = std::thread::spawn(move || engine.run(commands));
    loop {
        let event = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        if event["event"] == "chat_preview" {
            assert_eq!(event["preview"], "latest");
            break;
        }
    }
    assert_eq!(
        work.try_iter().collect::<Vec<_>>(),
        ["list_chats", "bridge_chat_preview"]
    );
    // Even a still-fresh old latest page must be fetched again after repair.
    tx.send(Command::Fetch {
        id: 2,
        chat: "chat".into(),
        before: None,
        generation: 1,
    })
    .unwrap();
    loop {
        let event = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        if event["id"] == 2 {
            assert_eq!(event["ok"], true);
            assert_eq!(event["result"]["messages"].as_array().unwrap().len(), 20);
            break;
        }
    }
    // Gap repair walks 160 -> 140 -> 120, reaching the original head 100.
    for _ in 0..3 {
        assert_eq!(
            work.recv_timeout(Duration::from_secs(5)).unwrap(),
            "bridge_fetch_page"
        );
    }
    // Query through the engine after the gap page has committed.
    tx.send(Command::Operation {
        id: 3,
        method: "reply_original".into(),
        params: json!({"chatMid":"chat","messageId":"101"}),
        generation: 1,
    })
    .unwrap();
    loop {
        let event = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        if event["id"] == 3 {
            assert_eq!(event["ok"], true, "{event}");
            assert_eq!(event["result"]["message"]["id"], "101");
            break;
        }
    }
    tx.send(Command::Stop).unwrap();
    worker.join().unwrap();
}

#[test]
fn sidebar_snapshot_preserves_history_boundaries_and_newer_preview() {
    let mut store = Store::open(Path::new(":memory:")).unwrap();
    store.set_account("self");
    store
        .save_catalog(
            "list_chats",
            &json!({"chats":[{"mid":"chat","lastActivity":100}]}),
        )
        .unwrap();
    store.save_page("chat", None, &page(100)).unwrap();
    store
        .save_chat_preview(
            "chat",
            &json!({"id":"160","createdTime":160,"contentType":"NONE","text":"latest"}),
        )
        .unwrap();
    assert!(store.message("chat", "160").unwrap().is_none());
    assert_eq!(
        store
            .page("chat", None, i64::MAX)
            .unwrap()
            .unwrap()
            .messages
            .len(),
        20
    );
    let mut catalog = store.catalog("list_chats", i64::MAX).unwrap().unwrap();
    store.decorate_chats(&mut catalog).unwrap();
    assert_eq!(catalog["chats"][0]["preview"], "latest");
    store.save_page("chat", None, &page(160)).unwrap();
    assert_eq!(store.gap("chat").unwrap().unwrap().target_id, "100");
    store.set_account("other");
    assert!(store.catalog("list_chats", i64::MAX).unwrap().is_none());
}

#[test]
fn stalled_preview_keeps_live_events_responsive_and_cannot_replace_newer_message() {
    struct Slow {
        events: mpsc::Sender<Value>,
        started: mpsc::Sender<()>,
        release: std::sync::Mutex<mpsc::Receiver<()>>,
    }
    impl Transport for Slow {
        fn call(&self, method: &str, _: Value, _: u64) -> Result<Value> {
            match method {
                "list_chats" => Ok(
                    json!({"chats":[{"mid":"chat","lastActivity":160,"lastMessage":{"messageId":"160","deliveredTime":"160"}}]}),
                ),
                "bridge_chat_preview" => {
                    self.started.send(())?;
                    self.release
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))?;
                    Ok(
                        json!({"messages":[{"id":"160","createdTime":160,"contentType":"NONE","text":"old snapshot"}]}),
                    )
                }
                _ => panic!("unexpected {method}"),
            }
        }
        fn materialize(&self, _: &Value, _: u64) -> Result<Value> {
            panic!("unexpected media")
        }
        fn emit(&self, event: Value) {
            let _ = self.events.send(event);
        }
    }
    let (events, rx) = mpsc::channel();
    let (started, start) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let (tx, commands) = mpsc::channel();
    let engine = Engine::new(
        Store::open(Path::new(":memory:")).unwrap(),
        Slow {
            events,
            started,
            release: std::sync::Mutex::new(wait),
        },
        Arc::new(AtomicU64::new(1)),
        "/nonexistent".into(),
    );
    tx.send(Command::Session {
        account: "self".into(),
        generation: 1,
    })
    .unwrap();
    tx.send(Command::Operation {
        id: 1,
        method: "list_chats".into(),
        params: json!({}),
        generation: 1,
    })
    .unwrap();
    let worker = std::thread::spawn(move || engine.run(commands));
    start.recv_timeout(Duration::from_secs(5)).unwrap();
    tx.send(Command::Live {
        chat: "chat".into(),
        message: json!({"id":"170","createdTime":170,"contentType":"NONE","text":"new live"}),
        generation: 1,
    })
    .unwrap();
    loop {
        let event = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        if event["event"] == "message" {
            assert_eq!(event["message"]["id"], "170");
            break;
        }
    }
    release.send(()).unwrap();
    assert!(
        rx.recv_timeout(Duration::from_millis(1500)).is_err(),
        "late preview must be discarded, with no repeat request"
    );
    tx.send(Command::Stop).unwrap();
    worker.join().unwrap();
}
