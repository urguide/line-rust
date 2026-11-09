/** Authenticated LINE Notes operations. Presentation stays in Rust. */
import type { Client } from "@evex/linejs";

type Json = Record<string, unknown>;
function object(value: unknown): Json {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as Json
    : {};
}
function identifier(value: unknown): string {
  if (typeof value !== "string" || !/^[a-zA-Z0-9_-]{1,200}$/.test(value)) {
    throw new Error("invalid_note_identifier");
  }
  return value;
}

/** Keep diagnostic text, never stringify response bodies or request headers. */
function errorDetail(envelope: Json, privateValues: unknown[]): string {
  const detail = [
    envelope.message,
    envelope.reason,
    envelope.errorMessage,
    object(envelope.error).message,
    object(envelope.error).reason,
  ]
    .filter((value): value is string =>
      typeof value === "string" && !!value.trim()
    );
  const secrets = privateValues.filter((v): v is string =>
    typeof v === "string" && !!v
  )
    .sort((a, b) => b.length - a.length);
  const parts = [...new Set(detail)].slice(0, 3).map((value) => {
    let text = value;
    for (const secret of secrets) text = text.split(secret).join("[redacted]");
    // Strip control characters before displaying server text in GTK/logs.
    text = Array.from(
      text,
      (c) => c.charCodeAt(0) < 32 || c.charCodeAt(0) === 127 ? " " : c,
    ).join("");
    return text
      .replace(/https?:\/\/[^\s<>"']+/gi, "[url]")
      .replace(/\b[ucgr][a-f0-9]{32}\b/gi, "[mid]")
      .replace(
        /\b(?:bearer\s+|(?:[\w-]*token|authorization|x-line-access|x-line-channeltoken|password|secret)\s*[=:]\s*)[^\s,;]+/gi,
        "[credential]",
      )
      .replace(/\b[A-Za-z0-9_+/=-]{40,}\b/g, "[opaque]")
      .replace(/\s+/g, " ").trim().slice(0, 240);
  });
  return parts.filter(Boolean).join("; ");
}

/** Note detail responses can wrap the post inside a feed. */
function postObject(value: unknown): Json {
  let row = object(value);
  let inheritedWriter: Json = {};
  const shapes: string[] = [];
  for (let depth = 0; depth < 4; depth++) {
    shapes.push(
      Object.keys(row).filter((k) => /^[a-zA-Z][a-zA-Z0-9_]{0,39}$/.test(k))
        .slice(0, 16).join(","),
    );
    const writer = object(row.writerInfo);
    if (writer.writerMid || writer.mid) inheritedWriter = writer;
    if (object(row.postInfo).postId != null) {
      // The feed envelope may own writerInfo while its post owns contents.
      // Keep the nearest explicit author; never infer ownership from homeId.
      return Object.keys(inheritedWriter).length
        ? { ...row, writerInfo: inheritedWriter }
        : row;
    }
    const nested = row.post ?? row.feed;
    if (!nested || typeof nested !== "object") break;
    row = object(nested);
  }
  throw new Error(
    `notes_invalid_post (post/get: missing postInfo.postId; fields=${
      shapes.join(" > ")
    })`,
  );
}

export async function notesRpc(
  c: Client,
  action: string,
  p: Json,
  transfer?: string,
) {
  const chat = identifier(p.chatMid);
  if (!/^[ucr]/.test(chat)) throw new Error("notes_chat_unsupported");
  const timeline = c.base.timeline;
  await timeline.initTimeline();
  async function request(
    path: string,
    params: Record<string, string>,
    body?: unknown,
  ): Promise<unknown> {
    const response = await c.base.fetch(
      `https://${c.base.request.endpoint}/ext/note/nt/api/${path}.json?${new URLSearchParams(
        params,
      )}`,
      {
        method: "POST",
        headers: {
          ...timeline.timelineHeaders,
          "x-lhm": body === undefined ? "GET" : "POST",
        },
        body: body === undefined ? undefined : JSON.stringify(body),
      },
    );
    let envelope: Json;
    try {
      envelope = object(await response.json());
    } catch {
      throw new Error(
        response.ok
          ? `notes_invalid_response (${path}: non-JSON response)`
          : `notes_http_${response.status} (${path})`,
      );
    }
    if (!response.ok || envelope.code !== 0) {
      const detail = errorDetail(envelope, [
        c.authToken,
        c.base.profile?.mid,
        chat,
        ...Object.values(params),
        p.text,
        p.postId,
        p.objectId,
        ...Object.entries(timeline.timelineHeaders)
          .filter(([key]) => /token|access|authorization|cookie/i.test(key))
          .map(([, value]) => value),
      ]);
      const code = typeof envelope.code === "number" ||
          (typeof envelope.code === "string" && /^\d+$/.test(envelope.code))
        ? String(envelope.code)
        : "invalid_response";
      const prefix = response.ok
        ? `notes_api_${code}`
        : `notes_http_${response.status}`;
      throw new Error(
        `${prefix} (${path})${detail ? `: ${detail}` : ""}`,
      );
    }
    if (!("result" in envelope)) throw new Error("notes_invalid_response");
    if (envelope.result === false) throw new Error("notes_operation_rejected");
    return envelope.result;
  }

  // A peer MID is a public home, NOT the private one-to-one notebook.
  // Resolve OTO every request so state never leaks between signed-in accounts.
  let home = chat;
  if (chat.startsWith("u")) {
    const own = identifier(c.base.profile?.mid);
    const sync = object(await request("v57/otoaccount/sync", { userMid: own }));
    if (!Array.isArray(sync.updated)) {
      throw new Error("notes_invalid_oto_response");
    }
    const match = sync.updated.map(object).find((item) =>
      item.userMid === chat
    );
    if (match) {
      home = identifier(match.groupId);
    } else if (action === "create") {
      // OTO creation carries the participants in both query and JSON body,
      // matching the Notes endpoint's request shape (CHRLINE createOtoAccount).
      const participants = { userMid: own, friendMid: chat };
      try {
        const created = object(
          await request("v57/otoaccount/create", participants, participants),
        );
        home = identifier(created.groupId);
      } catch (error) {
        // A rejected creation may race an already-created OTO home. Re-read
        // the mapping once; never retry the write or publish to a public MID.
        if (
          !(error instanceof Error) ||
          !error.message.startsWith("notes_api_501 ")
        ) throw error;
        let recovered: unknown;
        try {
          const refreshed = object(
            await request("v57/otoaccount/sync", { userMid: own }),
          );
          recovered = Array.isArray(refreshed.updated)
            ? refreshed.updated.map(object).find((item) =>
              item.userMid === chat
            )?.groupId
            : undefined;
        } catch { /* Preserve the original creation error. */ }
        if (
          typeof recovered !== "string" ||
          !/^[a-zA-Z0-9_-]{1,200}$/.test(recovered) || recovered.startsWith("u")
        ) throw error;
        home = recovered;
      }
    } else if (action === "list") {
      return { feeds: [] };
    } else {
      throw new Error("notes_not_found");
    }
    if (home.startsWith("u")) throw new Error("notes_invalid_oto_home");
  }
  const params = { homeId: home, sourceType: "TALKROOM" };
  const text = () => {
    if (typeof p.text !== "string" || !p.text.trim() || p.text.length > 10000) {
      throw new Error("notes_invalid_text");
    }
    return p.text;
  };
  if (action === "list") {
    const query: Record<string, string> = {
      ...params,
      likeLimit: "0",
      commentLimit: "0",
    };
    if (p.cursor != null) {
      const cursor = object(p.cursor);
      query.postId = identifier(cursor.postId);
      if (!/^\d+$/.test(String(cursor.updatedTime))) {
        throw new Error("notes_invalid_cursor");
      }
      query.updatedTime = String(cursor.updatedTime);
    }
    const result = object(await request("v57/post/list", query));
    if (Array.isArray(result.feeds)) {
      return { ...result, feeds: result.feeds.map(postObject) };
    }
    if (Array.isArray(result.posts)) {
      return { ...result, posts: result.posts.map(postObject) };
    }
    return result;
  }
  if (action === "create") {
    const content = typeof p.text === "string" ? p.text : "";
    const files = Array.isArray(p.files) ? p.files : [];
    if (
      (!content.trim() && files.length === 0) || content.length > 10000 ||
      files.length > 20
    ) {
      throw new Error("notes_invalid_text");
    }
    const media = [];
    for (const file of files) {
      const item = object(file);
      if (
        !transfer || typeof item.name !== "string" ||
        !/^[a-zA-Z0-9_-]+\.[a-zA-Z0-9]+$/.test(item.name)
      ) throw new Error("invalid_transfer_name");
      if (item.type !== "image" && item.type !== "video") {
        throw new Error("notes_invalid_media");
      }
      const bytes = await Deno.readFile(`${transfer}/${item.name}`);
      const uploaded = await timeline.uploadNoteMedia(
        item.type,
        new Blob([bytes]),
      );
      media.push({
        objectId: uploaded.objId,
        type: item.type === "image" ? "PHOTO" : "VIDEO",
        obsFace: "[]",
      });
    }
    return await request("v57/post/create", params, {
      postInfo: { readPermission: { type: "GROUP", gids: [home] } },
      contents: {
        contentsStyle: {
          textStyle: {
            textSizeMode: "NORMAL",
            backgroundColor: "#FFFFFF",
            textAnimation: "NONE",
          },
          mediaStyle: { displayType: "GRID_1_A" },
        },
        text: content,
        textMeta: [],
        stickers: [],
        locations: [],
        media,
      },
    });
  }
  const postId = identifier(p.postId);
  if (action === "media") {
    if (!transfer) throw new Error("notes_transfer_missing");
    const result = object(await request("v57/post/get", { ...params, postId }));
    const contents = object(postObject(result).contents);
    const media = Array.isArray(contents.media)
      ? contents.media.map(object)
      : [];
    const item = media.find((entry) => entry.objectId === p.objectId);
    if (!item || !["PHOTO", "VIDEO"].includes(String(item.type))) {
      throw new Error("notes_invalid_media");
    }
    const oid = identifier(item.objectId);
    const headers = Object.fromEntries(
      Object.entries(timeline.timelineHeaders).filter((
        entry,
      ): entry is [string, string] => typeof entry[1] === "string"),
    );
    const blob = await c.base.obs.downloadObjectForService({
      obsPath: "myhome/h",
      oid,
      addHeaders: headers,
    });
    const path = `${transfer}/${crypto.randomUUID()}.part`;
    const file = await Deno.open(path, {
      createNew: true,
      write: true,
      mode: 0o600,
    });
    try {
      await blob.stream().pipeTo(file.writable);
    } catch (error) {
      await Deno.remove(path).catch(() => {});
      throw error;
    }
    return { path, type: item.type, mime: blob.type };
  }
  if (action === "get") {
    const post = postObject(
      await request("v57/post/get", { ...params, postId }),
    );
    if (String(object(post.postInfo).postId) !== postId) {
      throw new Error("notes_invalid_post (post/get: ID mismatch)");
    }
    // A comment permission/network failure must not hide the post itself.
    let comments: unknown = null;
    let commentsError = false;
    try {
      comments = await request("v57/comment/getList", {
        ...params,
        contentId: postId,
      });
    } catch {
      commentsError = true;
    }
    return { post, comments, commentsError };
  }
  if (action === "comments") {
    const query: Record<string, string> = { ...params, contentId: postId };
    if (typeof p.scrollId === "string" && p.scrollId.length <= 4096) {
      query.scrollId = p.scrollId;
    }
    return await request("v57/comment/getList", query);
  }
  if (action === "comment") {
    return await request("v57/comment/create", params, {
      contentId: postId,
      commentText: text(),
      contentsList: [],
    });
  }
  if (action === "like") {
    if (p.liked === false) {
      return await request("v41/like/cancel", { ...params, contentId: postId });
    }
    return await request("v57/like/create", params, {
      sourceType: "TALKROOM",
      contentId: postId,
      actorId: home,
      likeType: "1003",
      sharable: false,
    });
  }
  if (action === "update" || action === "delete") {
    const result = object(await request("v57/post/get", { ...params, postId }));
    const post = postObject(result);
    const writer = object(post.writerInfo);
    const info = object(post.postInfo);
    const author = writer.writerMid || writer.mid;
    if (!author || !c.base.profile?.mid || author !== c.base.profile.mid) {
      throw new Error("notes_not_owner");
    }
    if (String(info.postId) !== postId) throw new Error("notes_invalid_post");
    if (action === "delete") {
      return await request("v57/post/delete", { ...params, postId });
    }
    // Patch the freshly fetched contents, retaining photos, video, stickers,
    // locations, styles and permissions instead of rebuilding a text-only post.
    if (!post.contents || typeof post.contents !== "object") {
      throw new Error("notes_invalid_post");
    }
    return await request("v57/post/update", params, {
      postInfo: {
        postId,
        editableContents: ["ALL"],
        ...(info.readPermission !== undefined
          ? { readPermission: info.readPermission }
          : {}),
      },
      contents: { ...object(post.contents), text: text(), textMeta: [] },
    });
  }
  throw new Error("notes_unknown_action");
}
