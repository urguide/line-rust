use super::*;
use serde_json::{Value, json};

const EMOJI: [&str; 6] = ["👍", "❤️", "😆", "😮", "😢", "😱"];
const KEYS: [&str; 6] = [
    "reaction_nice",
    "reaction_love",
    "reaction_fun",
    "reaction_amazing",
    "reaction_sad",
    "reaction_omg",
];

#[derive(Default)]
pub(super) struct ReactionUi {
    pub slots: HashMap<String, glib::WeakRef<gtk::Box>>,
    scheduled: bool,
    dirty: HashSet<String>,
    latest: HashMap<(String, String, String), Value>,
}

pub(super) fn update_message(message: &mut MessageInfo, row: &Value) {
    let at = row["reactionsUpdatedAt"].as_i64().unwrap_or(0);
    if at >= message.reactions_updated_at
        && row["reactions"].is_array()
        && let Ok(reactions) = serde_json::from_value(row["reactions"].clone())
    {
        message.reactions = Some(reactions);
        message.reactions_updated_at = at;
    }
}

pub(super) fn changed(state: &AppState) {
    if let Some(chat) = state.current_chat.borrow().clone() {
        state.reaction_ui.borrow_mut().dirty.insert(chat);
    }
    schedule(state);
}

fn schedule(state: &AppState) {
    if state.reaction_ui.borrow().scheduled {
        return;
    }
    state.reaction_ui.borrow_mut().scheduled = true;
    let s = state.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(180), move || {
        s.reaction_ui.borrow_mut().scheduled = false;
        sync(&s);
    });
}

pub(super) fn sync(state: &AppState) {
    if !*state.session_ready.borrow() {
        return;
    }
    let Some(chat) = state.current_chat.borrow().clone() else {
        return;
    };
    if state
        .pending
        .borrow()
        .values()
        .any(|p| matches!(p, Pending::Reactions { chat: c, message: None, .. } if c == &chat))
    {
        return;
    }
    let height = state.message_scroll.height() as f32;
    let visible: HashSet<_> = state
        .message_rows
        .borrow()
        .iter()
        .filter_map(|(id, row)| {
            let bounds = row.compute_bounds(&state.message_scroll)?;
            (bounds.y() < height && bounds.y() + bounds.height() > 0.0).then_some(id.clone())
        })
        .collect();
    let messages: Vec<_> = state
        .message_index
        .borrow()
        .values()
        .filter(|m| {
            visible.contains(&m.id) && !m.id.is_empty() && m.id.bytes().all(|b| b.is_ascii_digit())
        })
        .map(|m| json!({"id":m.id,"createdTime":m.created_time,"deliveredTime":m.delivered_time}))
        .collect();
    if state.message_index.borrow().is_empty() {
        return;
    }
    state.reaction_ui.borrow_mut().dirty.remove(&chat);
    if let Ok(id) = state.sidecar.request(
        "sync_reactions",
        Some(json!({"chatMid":chat,"messages":messages})),
    ) {
        state.pending.borrow_mut().insert(
            id,
            Pending::Reactions {
                chat,
                account: state.self_mid.borrow().clone(),
                message: None,
            },
        );
    }
}

pub(super) fn response(
    state: &AppState,
    chat: &str,
    account: Option<&str>,
    message: Option<&str>,
    ok: bool,
    error: Option<&str>,
) {
    if state.self_mid.borrow().as_deref() != account {
        return;
    }
    if message.is_some() {
        if !ok {
            toast(
                state,
                &crate::i18n::tf(
                    "reaction_failed",
                    &[("error", error.unwrap_or("request failed"))],
                ),
            );
        } else {
            changed(state);
        }
    } else if state.reaction_ui.borrow_mut().dirty.remove(chat) {
        schedule(state);
    }
}

pub(super) fn apply(state: &AppState, account: &str, chat: &str, rows: &Value) {
    if state.self_mid.borrow().as_deref() != Some(account) {
        return;
    }
    state.message_cache.borrow_mut().reactions(chat, rows);
    for row in rows.as_array().into_iter().flatten() {
        let Some(id) = row["id"].as_str() else {
            continue;
        };
        {
            let mut ui = state.reaction_ui.borrow_mut();
            if ui.latest.len() > 2000 {
                ui.latest.clear();
            }
            let key = (account.into(), chat.into(), id.into());
            if ui.latest.get(&key).is_some_and(|old| {
                old["reactionsUpdatedAt"].as_i64() > row["reactionsUpdatedAt"].as_i64()
            }) {
                continue;
            }
            ui.latest.insert(key, row.clone());
        }
        if state.current_chat.borrow().as_deref() != Some(chat) {
            continue;
        }
        if let Some(message) = state.message_index.borrow_mut().get_mut(id) {
            update_message(message, row);
        }
        let slot = state
            .reaction_ui
            .borrow()
            .slots
            .get(id)
            .and_then(|w| w.upgrade());
        if let Some(slot) = slot {
            render(state, &slot, row, chat, id);
        }
    }
}

fn render(state: &AppState, bar: &gtk::Box, row: &Value, chat: &str, id: &str) {
    let s = state.clone();
    let chat = chat.to_owned();
    let id = id.to_owned();
    render_chips(
        bar,
        row,
        state.self_mid.borrow().as_deref(),
        Rc::new(move |kind| send(&s, &chat, &id, kind)),
    );
}

fn render_chips(bar: &gtk::Box, row: &Value, account: Option<&str>, on_click: Rc<dyn Fn(u8)>) {
    while let Some(child) = bar.first_child() {
        bar.remove(&child);
    }
    let reactions: Vec<crate::protocol::ReactionInfo> =
        serde_json::from_value(row["reactions"].clone()).unwrap_or_default();
    for (i, emoji) in EMOJI.iter().enumerate() {
        let kind = i as u8 + 2;
        let users: Vec<_> = reactions.iter().filter(|r| r.reaction == kind).collect();
        if users.is_empty() {
            continue;
        }
        let own = users.iter().any(|r| account == Some(r.from.as_str()));
        let button = gtk::Button::builder()
            .label(format!("{emoji} {}", users.len()))
            .tooltip_text(crate::i18n::t(KEYS[i]))
            .css_classes(["flat", "line-reaction-chip"])
            .build();
        if own {
            button.add_css_class("line-reaction-mine");
        }
        let on_click = on_click.clone();
        button.connect_clicked(move |_| on_click(if own { 1 } else { kind }));
        bar.append(&button);
    }
    bar.set_visible(bar.first_child().is_some());
}

fn send(state: &AppState, chat: &str, message: &str, reaction: u8) {
    if !*state.session_ready.borrow()
        || state.current_chat.borrow().as_deref() != Some(chat)
        || state.unsent_msg_ids.borrow().contains(message)
        || state
            .pending
            .borrow()
            .values()
            .any(|p| matches!(p, Pending::Reactions { message: Some(id), .. } if id == message))
    {
        return;
    }
    match state.sidecar.request(
        "react",
        Some(json!({"chatMid":chat,"messageId":message,"reaction":reaction})),
    ) {
        Ok(id) => {
            state.pending.borrow_mut().insert(
                id,
                Pending::Reactions {
                    chat: chat.into(),
                    account: state.self_mid.borrow().clone(),
                    message: Some(message.into()),
                },
            );
        }
        Err(error) => toast(state, &error.to_string()),
    }
}

pub(super) fn append(
    state: &AppState,
    content: &gtk::Box,
    bubble: &gtk::Box,
    message: &MessageInfo,
) {
    if message.id.is_empty() || !message.id.bytes().all(|b| b.is_ascii_digit()) {
        return;
    }
    let Some(chat) = state.current_chat.borrow().clone() else {
        return;
    };
    let account = state.self_mid.borrow().clone().unwrap_or_default();
    let key = (account, chat.clone(), message.id.clone());
    let mut row = json!({"id":message.id,"reactions":message.reactions,"reactionsUpdatedAt":message.reactions_updated_at});
    if let Some(newer) = state.reaction_ui.borrow().latest.get(&key)
        && newer["reactionsUpdatedAt"].as_i64().unwrap_or(0) > message.reactions_updated_at
    {
        row = newer.clone();
    }
    let bar = gtk::Box::builder()
        .spacing(2)
        .halign(if message.mine {
            gtk::Align::End
        } else {
            gtk::Align::Start
        })
        .build();
    render(state, &bar, &row, &chat, &message.id);
    state
        .reaction_ui
        .borrow_mut()
        .slots
        .insert(message.id.clone(), bar.downgrade());
    let controls = gtk::Box::builder()
        .spacing(2)
        .halign(if message.mine {
            gtk::Align::End
        } else {
            gtk::Align::Start
        })
        .build();
    controls.append(&bar);
    content.append(&controls);
    if let Some(m) = state.message_index.borrow_mut().get_mut(&message.id) {
        update_message(m, &row);
    }

    let popover = gtk::Popover::new();
    let choices = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    for (i, emoji) in EMOJI.iter().enumerate() {
        let button = gtk::Button::builder()
            .label(*emoji)
            .tooltip_text(crate::i18n::t(KEYS[i]))
            .css_classes(["flat", "line-reaction-choice"])
            .build();
        let s = state.clone();
        let chat = chat.clone();
        let id = message.id.clone();
        let weak = popover.downgrade();
        button.connect_clicked(move |_| {
            if let Some(popover) = weak.upgrade() {
                popover.popdown();
            }
            let own = s.self_mid.borrow().clone();
            let kind = i as u8 + 2;
            let selected = s
                .message_index
                .borrow()
                .get(&id)
                .and_then(|m| m.reactions.as_ref())
                .is_some_and(|rs| {
                    rs.iter()
                        .any(|r| Some(r.from.as_str()) == own.as_deref() && r.reaction == kind)
                });
            send(&s, &chat, &id, if selected { 1 } else { kind });
        });
        choices.append(&button);
    }
    let undo = gtk::Button::builder()
        .icon_name("edit-undo-symbolic")
        .tooltip_text(crate::i18n::t("reaction_remove"))
        .css_classes(["flat"])
        .build();
    let s = state.clone();
    let id = message.id.clone();
    let chat2 = chat.clone();
    let weak = popover.downgrade();
    undo.connect_clicked(move |_| {
        if let Some(p) = weak.upgrade() {
            p.popdown();
        }
        send(&s, &chat2, &id, 1);
    });
    choices.append(&undo);
    popover.set_child(Some(&choices));
    let menu = gtk::MenuButton::builder()
        .icon_name("face-smile-symbolic")
        .tooltip_text(crate::i18n::t("reaction_add"))
        .css_classes(["flat", "circular", "line-reaction-menu"])
        .popover(&popover)
        .halign(gtk::Align::End)
        .build();
    // Keep an explicit control for images, links and keyboard navigation.
    controls.append(&menu);
    let click = gtk::GestureClick::new();
    click.set_button(1);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let start = Rc::new(RefCell::new((0.0_f64, 0.0_f64)));
    let start2 = start.clone();
    click.connect_pressed(move |_, _, x, y| *start2.borrow_mut() = (x, y));
    let weak_menu = menu.downgrade();
    let weak_bubble = bubble.downgrade();
    click.connect_released(move |_, n, x, y| {
        if n != 1 || (start.borrow().0 - x).abs() > 4.0 || (start.borrow().1 - y).abs() > 4.0 {
            return;
        }
        let Some(bubble) = weak_bubble.upgrade() else {
            return;
        };
        let mut target = bubble.pick(x, y, gtk::PickFlags::DEFAULT);
        while let Some(widget) = target {
            if widget == bubble {
                break;
            }
            if widget.is::<gtk::Button>()
                || widget.is::<gtk::Picture>()
                || widget.is::<gtk::TextView>()
            {
                return;
            }
            if let Some(label) = widget.downcast_ref::<gtk::Label>()
                && label.selection_bounds().is_some()
            {
                return;
            }
            target = widget.parent();
        }
        if let Some(menu) = weak_menu.upgrade() {
            menu.popup();
        }
    });
    bubble.add_controller(click);
    schedule(state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display; run under xvfb-run"]
    fn gtk_received_reactions_render_counts_change_and_clear_on_my_message() {
        gtk::init().unwrap();
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        let clicked = Rc::new(RefCell::new(Vec::new()));
        let sink = clicked.clone();
        let callback: Rc<dyn Fn(u8)> = Rc::new(move |kind| sink.borrow_mut().push(kind));
        let row = json!({"reactions":[
            {"fromUserMid":"friend1","reaction":3},
            {"fromUserMid":"friend2","reaction":3},
            {"fromUserMid":"me","reaction":2}
        ]});
        render_chips(&bar, &row, Some("me"), callback.clone());
        assert!(bar.is_visible());
        let first = bar
            .first_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        assert_eq!(first.label().as_deref(), Some("👍 1"));
        assert!(first.has_css_class("line-reaction-mine"));
        let second = first
            .next_sibling()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        assert_eq!(second.label().as_deref(), Some("❤️ 2"));
        assert!(!second.has_css_class("line-reaction-mine"));
        first.emit_clicked();
        second.emit_clicked();
        assert_eq!(*clicked.borrow(), vec![1, 3]);
        render_chips(
            &bar,
            &json!({"reactions":[{"fromUserMid":"friend1","reaction":6}]}),
            Some("me"),
            callback.clone(),
        );
        assert_eq!(
            bar.first_child()
                .unwrap()
                .downcast::<gtk::Button>()
                .unwrap()
                .label()
                .as_deref(),
            Some("😢 1")
        );
        render_chips(&bar, &json!({"reactions":[]}), Some("me"), callback);
        assert!(!bar.is_visible());
        assert!(bar.first_child().is_none());
    }
}
