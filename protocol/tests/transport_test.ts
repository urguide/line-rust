import { assertEquals, assertRejects } from "@std/assert";
import { BaseClient } from "@evex/linejs/base";
import { lineTransport } from "../src/transport.ts";

Deno.test("V3 authenticated RPCs use HTTPS directly instead of the failing /enc gateway", async () => {
  const requests: Request[] = [];
  const client = new BaseClient({
    device: "ANDROIDSECONDARY",
    ...lineTransport,
    fetch: (input) => {
      requests.push(new Request(input));
      return Promise.reject(new Error("offline transport probe"));
    },
  });
  // A synthetic JWT-shaped credential exercises the V3 transport selection.
  client.authToken = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ0ZXN0In0.signature";
  for (const path of ["/S4", "/SYNC4", "/acct/lgn/sq/v1"]) {
    await assertRejects(
      () => client.request.request([], "probe", 4, false, path),
      Error,
      "offline transport probe",
    );
    const request = requests.at(-1)!;
    assertEquals(request.url, `https://legy.line-apps.com${path}`);
    assertEquals(request.headers.get("x-line-access"), client.authToken);
  }
  assertEquals(requests.length, 3);
});
