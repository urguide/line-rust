import type { Client } from "@evex/linejs";
import { coerceI64 } from "./messages.ts";
import { packRaw } from "./raw_codec.ts";

type Json = Record<string, unknown>;
const types = ["UNDO", "NICE", "LOVE", "FUN", "AMAZING", "SAD", "OMG"] as const;

/** Operations invalidate snapshots; never guess counts from undocumented params. */
export function isReactionOperation(op: { type?: string | number }): boolean {
  return ["139", "140", "SEND_REACTION", "NOTIFIED_SEND_REACTION"].includes(
    String(op.type),
  );
}

export async function sendReaction(c: Client, p: Json) {
  const id = String(p.messageId ?? "");
  const reaction = Number(p.reaction);
  if (
    !/^\d+$/.test(id) || !Number.isInteger(reaction) || reaction < 1 ||
    reaction > 7
  ) {
    throw new Error("invalid_reaction");
  }
  if (reaction === 1) {
    await c.base.talk.cancelReaction({
      cancelReactionRequest: {
        reqSeq: await c.base.getReqseq(),
        messageId: coerceI64(id),
      },
    });
  } else {
    await c.base.talk.react({
      id: coerceI64(id),
      reaction: types[reaction - 1],
    });
  }
  return { sent: true };
}

/** Read raw reaction snapshots only: no E2EE decode, media or history changes. */
export async function reactionSnapshots(c: Client, p: Json) {
  const chat = String(p.chatMid ?? "");
  if (!/^[ucr][a-zA-Z0-9]+$/.test(chat)) {
    throw new Error("invalid_reaction_chat");
  }
  const at = Date.now();
  const messages = await c.base.talk.getRecentMessagesV2({
    messageBoxId: chat,
    messagesCount: 20,
  });
  const found = new Map(messages.map((m) => [String(m.id), m]));
  // Visible historical messages are refreshed too, including messages older
  // than the latest page. Use inclusive history cursors, 20 messages per call.
  const requested =
    (Array.isArray(p.messages) ? p.messages.slice(0, 100) as Json[] : [])
      .filter((m) => m && /^\d+$/.test(String(m.id)));
  requested.sort((a, b) => {
    const x = BigInt(String(a.id));
    const y = BigInt(String(b.id));
    return x < y ? 1 : x > y ? -1 : 0;
  });
  for (const item of requested) {
    const id = String(item.id);
    if (!/^\d+$/.test(id) || found.has(id)) continue;
    const time = String(item.deliveredTime || item.createdTime || "");
    if (!/^\d+$/.test(time) || BigInt(time) <= 0n) continue;
    const page = await c.base.talk.getPreviousMessagesV2WithRequest({
      request: {
        messageBoxId: chat,
        endMessageId: {
          messageId: coerceI64(id),
          deliveredTime: coerceI64(time),
        },
        messagesCount: 20,
        withReadCount: false,
      },
    });
    for (const m of page) found.set(String(m.id), m);
  }
  return {
    at,
    messages: [...found.values()].map((m) => ({
      id: String(m.id),
      reactions: packRaw(m.reactions ?? []),
    })),
  };
}
