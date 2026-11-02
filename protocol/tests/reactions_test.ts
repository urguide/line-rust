import { assertEquals, assertRejects } from "@std/assert";
import type { Client } from "@evex/linejs";
import {
  isReactionOperation,
  reactionSnapshots,
  sendReaction,
} from "../src/reactions.ts";
import { createApi } from "../src/api.ts";

Deno.test("all six reactions and cancellation preserve 64-bit IDs and propagate rejection", async () => {
  const calls: unknown[] = [];
  const c = {
    base: {
      getReqseq: () => Promise.resolve(42),
      talk: {
        react: (p: unknown) => {
          calls.push(p);
          return Promise.resolve();
        },
        cancelReaction: (p: unknown) => {
          calls.push(p);
          return Promise.resolve();
        },
      },
    },
  } as unknown as Client;
  for (const reaction of [2, 3, 4, 5, 6, 7, 1]) {
    await sendReaction(c, { messageId: "9007199254740993", reaction });
  }
  assertEquals(
    calls.slice(0, 6),
    ["NICE", "LOVE", "FUN", "AMAZING", "SAD", "OMG"].map((reaction) => ({
      id: 9007199254740993n,
      reaction,
    })),
  );
  assertEquals(calls[6], {
    cancelReactionRequest: { messageId: 9007199254740993n, reqSeq: 42 },
  });
  for (const reaction of [0, 8, 1.5, "bad"]) {
    await assertRejects(() => sendReaction(c, { messageId: "1", reaction }));
  }
  c.base.talk.react = () => Promise.reject(new Error("expired"));
  await assertRejects(
    () => sendReaction(c, { messageId: "1", reaction: 2 }),
    Error,
    "expired",
  );
});

Deno.test("reaction snapshots include own messages and visible history with empty cancellation snapshots", async () => {
  const history: unknown[] = [];
  const c = {
    base: {
      talk: {
        getRecentMessagesV2: () =>
          Promise.resolve([{
            id: "9007199254740993",
            from: "me",
            reactions: [{
              fromUserMid: "friend",
              atMillis: 12n,
              reactionType: { predefinedReactionType: "LOVE" },
            }],
          }]),
        getPreviousMessagesV2WithRequest: (p: unknown) => {
          history.push(p);
          return Promise.resolve([{ id: "12", reactions: [] }, {
            id: "11",
            reactions: [],
          }]);
        },
      },
    },
  } as unknown as Client;
  const result = await reactionSnapshots(c, {
    chatMid: "upeer",
    messages: [{ id: "12", deliveredTime: "9007199254740994" }, {
      id: "11",
      createdTime: 123,
    }, { id: "bad" }],
  });
  assertEquals(history.length, 1);
  assertEquals(history[0], {
    request: {
      messageBoxId: "upeer",
      endMessageId: { messageId: 12, deliveredTime: 9007199254740994n },
      messagesCount: 20,
      withReadCount: false,
    },
  });
  assertEquals(result.messages[0], {
    id: "9007199254740993",
    reactions: [{
      fromUserMid: "friend",
      atMillis: "12",
      reactionType: { predefinedReactionType: "LOVE" },
    }],
  });
  assertEquals(result.messages[1], { id: "12", reactions: [] });
  c.base.talk.getRecentMessagesV2 = () => Promise.reject(new Error("offline"));
  await assertRejects(
    () => reactionSnapshots(c, { chatMid: "upeer" }),
    Error,
    "offline",
  );
});

Deno.test("live reaction operations invalidate snapshots and reaction writes stay exclusive", async () => {
  for (const type of [139, 140, "SEND_REACTION", "NOTIFIED_SEND_REACTION"]) {
    assertEquals(isReactionOperation({ type }), true);
  }
  assertEquals(isReactionOperation({ type: "NOTIFIED_READ_MESSAGE" }), false);
  let release!: () => void;
  const c = {
    base: { talk: { react: () => new Promise<void>((r) => release = r) } },
  } as unknown as Client;
  const api = createApi(() => c, "/unused");
  const first = api("bridge_react", { messageId: "1", reaction: 2 });
  await assertRejects(
    () => api("bridge_react", { messageId: "1", reaction: 3 }),
    Error,
    "bridge_busy",
  );
  release();
  await first;
});
