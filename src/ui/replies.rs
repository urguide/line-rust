use super::*;

#[derive(Default)]
pub(super) struct ReplyUi {
    originals: HashMap<String, MessageInfo>,
    requested: HashSet<String>,
    slots: HashMap<String, Vec<glib::WeakRef<gtk::Box>>>,
    jump: std::rc::Rc<std::cell::Cell<u64>>,
}

const UNAVAILABLE: &str = "無法讀取";

pub(super) fn append(state: &AppState, content: &gtk::Box, id: &str) {
    let quote = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::Start)
        .css_classes(["caption"])
        .build();
    content.append(&quote);
    state
        .reply_ui
        .borrow_mut()
        .slots
        .entry(id.to_owned())
        .or_default()
        .push(quote.downgrade());
    render(state, &quote, id);
    let s = state.clone();
    let original_id = id.to_owned();
    quote.connect_map(move |quote| render(&s, quote, &original_id));
    let click = gtk::GestureClick::new();
    click.set_button(1);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let s = state.clone();
    let original_id = id.to_owned();
    let chat = state.current_chat.borrow().clone();
    click.connect_released(move |gesture, count, _, _| {
        if count == 1 && *s.current_chat.borrow() == chat {
            gesture.set_state(gtk::EventSequenceState::Claimed);
            jump_to_original(&s, &original_id);
        }
    });
    quote.set_cursor_from_name(Some("pointer"));
    quote.add_controller(click);

    if original(state, id).is_none()
        && !state.unsent_msg_ids.borrow().contains(id)
        && state.reply_ui.borrow_mut().requested.insert(id.to_owned())
        && let Some(chat) = state.current_chat.borrow().clone()
        && let Ok(request) = state.sidecar.request(
            "reply_original",
            Some(serde_json::json!({
                "chatMid":chat, "messageId":id,
            })),
        )
    {
        state.pending.borrow_mut().insert(
            request,
            Pending::ReplyOriginal {
                message_id: id.to_owned(),
                generation: *state.message_generation.borrow(),
            },
        );
    }
}

fn original(state: &AppState, id: &str) -> Option<MessageInfo> {
    if state.unsent_msg_ids.borrow().contains(id) {
        return None;
    }
    let mut msg = state
        .media_msgs
        .borrow()
        .get(id)
        .cloned()
        .or_else(|| state.message_index.borrow().get(id).cloned())
        .or_else(|| state.reply_ui.borrow().originals.get(id).cloned())?;
    let memory = state
        .current_chat
        .borrow()
        .as_deref()
        .is_some_and(|chat| chat_memory_media(state, chat));
    if memory {
        msg.image_path = msg
            .image_path
            .filter(|p| crate::memory_media::is_ram_path(p));
        msg.file_path = msg
            .file_path
            .filter(|p| crate::memory_media::is_ram_path(p));
    }
    if let Some(path) = state.media_ready_paths.borrow().get(id)
        && (!memory || crate::memory_media::is_ram_path(path))
    {
        msg.image_path = Some(path.clone());
    }
    Some(msg)
}

fn unavailable(quote: &gtk::Box) {
    while let Some(child) = quote.first_child() {
        quote.remove(&child);
    }
    let label = gtk::Label::new(Some(UNAVAILABLE));
    label.add_css_class("dim-label");
    quote.append(&label);
}

/// Return existing original/preview paths only. A surviving video thumbnail is
/// not a playable original, and must not hide an expired/deleted video.
fn cached_media_paths(msg: &MessageInfo) -> Option<(Option<&str>, &str)> {
    let preview = msg
        .image_path
        .as_deref()
        .filter(|p| std::path::Path::new(p).is_file());
    let file = msg
        .file_path
        .as_deref()
        .filter(|p| std::path::Path::new(p).is_file());
    if msg.content_type.eq_ignore_ascii_case("VIDEO") {
        Some((preview, file?))
    } else {
        Some((preview.or(file), file.or(preview)?))
    }
}

fn render(state: &AppState, quote: &gtk::Box, id: &str) {
    while let Some(child) = quote.first_child() {
        quote.remove(&child);
    }
    let Some(msg) = original(state, id) else {
        unavailable(quote);
        return;
    };
    let identity = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .valign(gtk::Align::Start)
        .build();
    senders::append_reply_identity(state, &identity, &msg);
    quote.append(&identity);
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .css_classes(["line-reply-preview"])
        .build();
    quote.append(&body);
    let quote = &body;
    quote.append(
        &gtk::Label::builder()
            .label(crate::i18n::tf(
                "replying_to",
                &[("message", &reply_summary(&msg))],
            ))
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(42)
            .build(),
    );
    let video = msg.content_type.eq_ignore_ascii_case("VIDEO");
    if !video && !msg.content_type.eq_ignore_ascii_case("IMAGE") {
        return;
    }
    let Some((path, original_path)) = cached_media_paths(&msg) else {
        unavailable(quote);
        return;
    };
    if let Some(path) = path {
        let picture = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Contain)
            .can_shrink(true)
            .width_request(160)
            .height_request(100)
            .build();
        quote.append(&picture);
        let weak = picture.downgrade();
        let path = path.to_owned();
        let lease = crate::memory_media::lease(&path);
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _lease = lease;
            let _ = tx.send_blocking(crate::sticker_anim::load_scaled(&path, 160, false));
        });
        glib::spawn_future_local(async move {
            let frames = rx.recv().await.ok().flatten();
            let Some(picture) = weak.upgrade() else {
                return;
            };
            if let Some(frames) = frames {
                apply_frames_to_picture(&picture, frames, false);
            } else if let Some(parent) = picture.parent().and_downcast::<gtk::Box>() {
                unavailable(&parent);
            }
        });
        if video {
            quote.append(&gtk::Label::new(Some("▶")));
        }
    } else if video {
        // Reuse the original video directly if no thumbnail survives. GTK's
        // paused video preview does not write a second thumbnail to disk.
        let player = gtk::Video::for_filename(Some(original_path));
        player.set_autoplay(false);
        player.set_size_request(160, 100);
        if let Some(stream) = player.media_stream() {
            let weak = quote.downgrade();
            stream.connect_error_notify(move |stream| {
                if stream.error().is_some()
                    && let Some(quote) = weak.upgrade()
                {
                    unavailable(&quote);
                }
            });
        }
        quote.append(&player);
    } else {
        unavailable(quote);
    }
}

fn message_bubble(widget: &gtk::Widget) -> Option<gtk::Widget> {
    if widget.has_css_class("line-bubble") {
        return Some(widget.clone());
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        if let Some(bubble) = message_bubble(&widget) {
            return Some(bubble);
        }
        child = widget.next_sibling();
    }
    None
}

pub(super) fn jump_to_original(state: &AppState, id: &str) {
    let token = state.reply_ui.borrow().jump.clone();
    let request = token.get().wrapping_add(1);
    token.set(request);
    let generation = *state.history_generation.borrow();
    let message_generation = *state.message_generation.borrow();
    let s = state.clone();
    let id = id.to_owned();
    let start = std::time::Instant::now();
    let mut previous_cursor = None;
    let mut positioned = false;
    *state.stick_bottom.borrow_mut() = false;
    *state.scroll_pin_gen.borrow_mut() += 1;
    if !*state.history_loading.borrow() {
        *state.scroll_pinning.borrow_mut() = false;
    }
    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        if token.get() != request
            || *s.history_generation.borrow() != generation
            || *s.message_generation.borrow() != message_generation
            || !*s.session_ready.borrow()
        {
            return glib::ControlFlow::Break;
        }
        if s.unsent_msg_ids.borrow().contains(&id) || start.elapsed().as_secs() >= 30 {
            toast(&s, &crate::i18n::t("reply_original_unavailable"));
            return glib::ControlFlow::Break;
        }
        if *s.history_loading.borrow() || *s.scroll_pinning.borrow() {
            return glib::ControlFlow::Continue;
        }
        let row = s.message_rows.borrow().get(&id).cloned();
        if let Some(row) = row {
            if !positioned {
                positioned = s.message_list.scroll_to_row(&row);
                return glib::ControlFlow::Continue;
            }
            if !row.is_mapped() {
                return glib::ControlFlow::Continue;
            }
            if let Some(bounds) = row.compute_bounds(&s.message_scroll) {
                let adj = s.message_scroll.vadjustment();
                let value = adj.value() + f64::from(bounds.y())
                    - ((adj.page_size() - f64::from(bounds.height())) / 2.0).max(0.0);
                adj.set_value(value.clamp(
                    adj.lower(),
                    (adj.upper() - adj.page_size()).max(adj.lower()),
                ));
            }
            if let Some(bubble) = message_bubble(row.upcast_ref()) {
                bubble.remove_css_class("line-reply-shake");
                let weak = bubble.downgrade();
                glib::timeout_add_local_once(std::time::Duration::from_millis(20), move || {
                    if let Some(bubble) = weak.upgrade() {
                        bubble.add_css_class("line-reply-shake");
                        let weak = bubble.downgrade();
                        glib::timeout_add_local_once(
                            std::time::Duration::from_secs(1),
                            move || {
                                if let Some(bubble) = weak.upgrade() {
                                    bubble.remove_css_class("line-reply-shake");
                                }
                            },
                        );
                    }
                });
            }
            return glib::ControlFlow::Break;
        }
        let cursor = s.history_cursor.borrow().clone();
        if !*s.history_has_more.borrow() || cursor.is_none() || cursor == previous_cursor {
            toast(&s, &crate::i18n::t("reply_original_unavailable"));
            return glib::ControlFlow::Break;
        }
        previous_cursor = cursor;
        load_older_messages(&s);
        glib::ControlFlow::Continue
    });
}

pub(super) fn refresh(state: &AppState, id: &str) {
    let slots: Vec<_> = state
        .reply_ui
        .borrow()
        .slots
        .get(id)
        .into_iter()
        .flatten()
        .filter_map(|slot| slot.upgrade())
        .collect();
    for quote in slots {
        render(state, &quote, id);
    }
}

pub(super) fn refresh_all(state: &AppState) {
    let ids: Vec<_> = state.reply_ui.borrow().slots.keys().cloned().collect();
    for id in ids {
        refresh(state, &id);
    }
}

pub(super) fn response(
    state: &AppState,
    id: &str,
    generation: u64,
    ok: bool,
    result: &serde_json::Value,
) {
    if generation != *state.message_generation.borrow()
        || state.unsent_msg_ids.borrow().contains(id)
    {
        return;
    }
    if ok
        && let Ok(msg) = serde_json::from_value::<MessageInfo>(result["message"].clone())
        && msg.id == id
    {
        state
            .reply_ui
            .borrow_mut()
            .originals
            .insert(id.to_owned(), msg);
    }
    refresh(state, id);
}

pub(super) fn media_ready(state: &AppState, id: &str, image: &str, file: Option<&str>) {
    if let Some(msg) = state.reply_ui.borrow_mut().originals.get_mut(id) {
        if !image.is_empty() {
            msg.image_path = Some(image.to_owned());
        }
        if let Some(file) = file {
            msg.file_path = Some(file.to_owned());
        }
    }
    if let Some(msg) = state.message_index.borrow_mut().get_mut(id) {
        if !image.is_empty() {
            msg.image_path = Some(image.to_owned());
        }
        if let Some(file) = file {
            msg.file_path = Some(file.to_owned());
        }
    }
    refresh(state, id);
}

pub(super) fn release_media(state: &AppState, id: &str) {
    if let Some(msg) = state.reply_ui.borrow_mut().originals.get_mut(id) {
        msg.image_path = None;
        msg.file_path = None;
    }
    if let Some(msg) = state.message_index.borrow_mut().get_mut(id) {
        msg.image_path = None;
        msg.file_path = None;
    }
    refresh(state, id);
}

pub(super) fn visible_media(state: &AppState, height: f32) -> Vec<String> {
    let ui = state.reply_ui.borrow();
    ui.slots
        .iter()
        .filter(|(_, slots)| {
            slots.iter().filter_map(|s| s.upgrade()).any(|quote| {
                quote.is_mapped()
                    && quote
                        .compute_bounds(&state.message_scroll)
                        .is_some_and(|bounds| {
                            bounds.y() < height && bounds.y() + bounds.height() > 0.0
                        })
            })
        })
        .map(|(id, _)| id.clone())
        .collect()
}

pub(super) fn invalidate(state: &AppState, id: &str) {
    state.reply_ui.borrow_mut().originals.remove(id);
    refresh(state, id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_share_original_files_and_detect_removed_video_despite_surviving_thumbnail() {
        let dir = std::env::temp_dir().join(format!("line-reply-paths-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let image = dir.join("original.png");
        let video = dir.join("original.mp4");
        std::fs::write(&image, b"preview fixture").unwrap();
        std::fs::write(&video, b"video fixture").unwrap();
        let mut msg: MessageInfo = serde_json::from_value(serde_json::json!({
            "id":"original", "contentType":"VIDEO", "imagePath":image, "filePath":video,
        }))
        .unwrap();
        for _ in 0..3 {
            assert_eq!(
                cached_media_paths(&msg),
                Some((image.to_str(), video.to_str().unwrap()))
            );
        }
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        std::fs::remove_file(&video).unwrap();
        assert!(cached_media_paths(&msg).is_none());
        msg.content_type = "IMAGE".into();
        msg.file_path = None;
        assert_eq!(
            cached_media_paths(&msg),
            Some((image.to_str(), image.to_str().unwrap()))
        );
        std::fs::remove_file(&image).unwrap();
        assert!(cached_media_paths(&msg).is_none());
        std::fs::remove_dir(&dir).unwrap();
    }
}
