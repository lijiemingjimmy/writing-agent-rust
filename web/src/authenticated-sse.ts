/** SSE over fetch so credentials stay in headers rather than URLs. */
export class AuthenticatedEventSource {
  onerror: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent<string>) => void) | null = null;
  private listeners = new Map<string, Array<(event: MessageEvent<string>) => void>>();
  private controller = new AbortController();
  private closed = false;

  constructor(url: string, fetcher: typeof fetch) {
    void this.read(url, fetcher);
  }

  addEventListener(type: string, listener: (event: MessageEvent<string>) => void) {
    const listeners = this.listeners.get(type) ?? [];
    listeners.push(listener);
    this.listeners.set(type, listeners);
  }

  close() {
    this.closed = true;
    this.controller.abort();
  }

  private async read(url: string, fetcher: typeof fetch) {
    let reader: ReadableStreamDefaultReader<Uint8Array> | undefined;
    try {
      const response = await fetcher(url, {
        headers: { Accept: "text/event-stream" }, signal: this.controller.signal
      });
      if (!response.ok || !response.body) throw new Error("SSE request failed");
      reader = response.body.getReader();
      const decoder = new TextDecoder();
      let buffer = "";
      let kind = "message";
      let data: string[] = [];
      while (!this.closed) {
        const chunk = await reader.read();
        if (chunk.done) break;
        buffer += decoder.decode(chunk.value, { stream: true });
        // Bound malformed streams which never produce line delimiters.
        if (buffer.length > 8 * 1024 * 1024) throw new Error("SSE frame too large");
        let newline: number;
        while (!this.closed && (newline = buffer.indexOf("\n")) >= 0) {
          const line = buffer.slice(0, newline).replace(/\r$/, "");
          buffer = buffer.slice(newline + 1);
          if (line === "") {
            if (data.length) {
              const event = new MessageEvent<string>(kind, { data: data.join("\n") });
              for (const listener of this.listeners.get(kind) ?? []) listener(event);
              if (kind === "message") this.onmessage?.(event);
            }
            kind = "message";
            data = [];
          } else if (!line.startsWith(":")) {
            const colon = line.indexOf(":");
            const field = colon < 0 ? line : line.slice(0, colon);
            const value = colon < 0 ? "" : line.slice(colon + 1).replace(/^ /, "");
            if (field === "event") kind = value || "message";
            if (field === "data") data.push(value);
          }
        }
      }
      if (!this.closed) this.onerror?.(new Event("error"));
    } catch {
      if (!this.closed) this.onerror?.(new Event("error"));
    } finally {
      await reader?.cancel().catch(() => {});
    }
  }
}
