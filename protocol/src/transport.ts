/** Use LINE's HTTPS RPC endpoints; message E2EE remains enabled independently.
 * The optional LEGY /enc envelope returns empty HTTP 404s (linejs issue #280).
 * Apply this to both new QR sessions and restored tokens, including live RPCs.
 */
export const lineTransport = { legy: { encrypted: false } } as const;
