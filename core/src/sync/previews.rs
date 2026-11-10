//! Low-cost sidebar repair: one summary snapshot, then one message per changed chat.
//! This lane never saves history pages, downloads media or sends read receipts.
use super::*;

const INTERVAL: Duration = Duration::from_secs(300);
const PAUSE: Duration = Duration::from_secs(2);

enum Completion {
    Boxes(u64, Result<Value>),
    Preview(u64, String, Cursor, Result<Value>),
}

pub(super) struct Previews {
    enabled: bool,
    running: bool,
    next_check: Instant,
    next_request: Instant,
    queue: VecDeque<(String, Cursor)>,
    tx: mpsc::Sender<Completion>,
    rx: mpsc::Receiver<Completion>,
}

impl Default for Previews {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            enabled: false,
            running: false,
            next_check: Instant::now(),
            next_request: Instant::now(),
            queue: VecDeque::new(),
            tx,
            rx,
        }
    }
}

impl<T: Transport> Engine<T> {
    pub(super) fn enable_previews(
        &mut self,
        catalog: &Value,
        generation: u64,
        fresh: bool,
    ) -> Result<()> {
        if !catalog["chats"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
        {
            return Ok(());
        }
        self.previews.enabled = true;
        if fresh && catalog["partial"] != true {
            self.previews.next_check = Instant::now() + INTERVAL;
        }
        // A fresh catalog already contains the message-box snapshot. Reuse it.
        if fresh
            && catalog["chats"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|r| r["lastMessage"].is_object()))
        {
            self.queue_previews(catalog, generation)?;
        }
        Ok(())
    }

    fn queue_previews(&mut self, catalog: &Value, generation: u64) -> Result<()> {
        self.valid(generation)?;
        let mut rows: Vec<_> = catalog["chats"].as_array().into_iter().flatten().collect();
        rows.sort_by_key(|r| std::cmp::Reverse(crate::message::integer(&r["lastActivity"])));
        for row in rows {
            let chat = crate::message::string(&row["mid"]);
            let Ok(cursor) = serde_json::from_value::<Cursor>(row["lastMessage"].clone()) else {
                continue;
            };
            if chat.is_empty()
                || cursor.validate().is_err()
                || row["previewMessageId"] == cursor.message_id
                || crate::message::integer(&row["previewTime"])
                    > cursor.delivered_time.parse::<i64>().unwrap_or(0)
            {
                continue;
            }
            // Expire only the latest page. Keep heads and historical cursors so
            // opening the chat still detects and repairs the entire missing range.
            self.store.invalidate(&chat)?;
            if let Some(queued) = self.previews.queue.iter_mut().find(|(mid, _)| mid == &chat) {
                queued.1 = cursor;
            } else {
                self.previews.queue.push_back((chat, cursor));
            }
        }
        Ok(())
    }

    fn publish_preview(&mut self, chat: &str, cursor: &Cursor, message: &Value) -> Result<()> {
        if self.store.message_withdrawn(id(message))? {
            return Ok(());
        }
        let catalog = self
            .store
            .stale_catalog("list_chats", self.policy.disk_chats)?
            .unwrap_or_default();
        let Some(row) = catalog["chats"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|r| r["mid"] == chat)
        else {
            return Ok(());
        };
        if crate::message::integer(&row["previewTime"])
            > crate::message::integer(&message["createdTime"])
            || self.store.latest_message(chat)?.is_some_and(|local| {
                crate::message::integer(&local["createdTime"])
                    > crate::message::integer(&message["createdTime"])
            })
        {
            return Ok(());
        }
        self.store.save_chat_preview(chat, message)?;
        self.transport.emit(json!({"event":"chat_preview","mid":chat,
            "preview":crate::message::preview(message["contentType"].as_str().unwrap_or_default(), message["text"].as_str().unwrap_or_default()),
            "lastActivity":crate::message::integer(&message["createdTime"]).max(cursor.delivered_time.parse::<i64>().unwrap_or(0))}));
        Ok(())
    }

    pub(super) fn finish_previews(&mut self) -> Result<()> {
        while let Ok(done) = self.previews.rx.try_recv() {
            self.previews.running = false;
            self.previews.next_request = Instant::now() + PAUSE;
            match done {
                Completion::Boxes(generation, result) => {
                    self.valid(generation)?;
                    let boxes = result?;
                    let mut catalog = self
                        .store
                        .stale_catalog("list_chats", self.policy.disk_chats)?
                        .unwrap_or_default();
                    for row in catalog["chats"].as_array_mut().into_iter().flatten() {
                        if let Some(box_) = boxes["messageBoxes"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .find(|b| b["id"] == row["mid"])
                        {
                            row["lastMessage"] = json!({
                                "messageId":crate::message::string(&box_["lastDeliveredMessageId"]["messageId"]),
                                "deliveredTime":crate::message::string(&box_["lastDeliveredMessageId"]["deliveredTime"]),
                            });
                            row["lastActivity"] = json!(crate::message::integer(
                                &box_["lastDeliveredMessageId"]["deliveredTime"]
                            ));
                        }
                    }
                    self.queue_previews(&catalog, generation)?;
                }
                Completion::Preview(generation, chat, cursor, result) => {
                    self.valid(generation)?;
                    let value = result?;
                    if let Some(raw) = value["messages"].as_array().and_then(|rows| rows.first()) {
                        let message = crate::message::from_envelope(raw, &self.account);
                        // A late response must not overwrite an event received during the request.
                        if let Some(local) = self.store.message(&chat, &cursor.message_id)? {
                            self.publish_preview(&chat, &cursor, &local)?;
                        } else if id(&message) == cursor.message_id
                            && message["decodeError"].is_null()
                        {
                            self.publish_preview(&chat, &cursor, &message)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn start_preview(&mut self) -> Result<()> {
        let generation = self.session_generation;
        if !self.previews.enabled
            || self.previews.running
            || self.valid(generation).is_err()
            || Instant::now() < self.previews.next_request
        {
            return Ok(());
        }
        // A single slow reconciliation request cannot block the core event loop.
        while let Some((chat, cursor)) = self.previews.queue.pop_front() {
            let catalog = self
                .store
                .stale_catalog("list_chats", self.policy.disk_chats)?
                .unwrap_or_default();
            let row = catalog["chats"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|r| r["mid"] == chat);
            if row.is_none()
                || row.is_some_and(|r| r["previewMessageId"] == cursor.message_id)
                || row.is_some_and(|r| {
                    crate::message::integer(&r["previewTime"])
                        > cursor.delivered_time.parse::<i64>().unwrap_or(0)
                })
                || self.store.message_withdrawn(&cursor.message_id)?
            {
                continue;
            }
            if let Some(local) = self.store.message(&chat, &cursor.message_id)? {
                self.publish_preview(&chat, &cursor, &local)?;
                continue;
            }
            let transport = self.transport.clone();
            let tx = self.previews.tx.clone();
            self.previews.running = true;
            thread::spawn(move || {
                let result = transport.call(
                    "bridge_chat_preview",
                    json!({"chatMid":chat,"cursor":cursor}),
                    generation,
                );
                let _ = tx.send(Completion::Preview(generation, chat, cursor, result));
            });
            return Ok(());
        }
        if Instant::now() >= self.previews.next_check {
            self.previews.next_check = Instant::now() + INTERVAL;
            self.previews.running = true;
            let transport = self.transport.clone();
            let tx = self.previews.tx.clone();
            thread::spawn(move || {
                let result = transport.call("bridge_boxes", json!({}), generation);
                let _ = tx.send(Completion::Boxes(generation, result));
            });
        }
        Ok(())
    }
}
