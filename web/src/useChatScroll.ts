import { useCallback, useLayoutEffect, useRef, useState } from "react";

/** Follow new content unless the reader deliberately scrolls up. */
export function useChatScroll(sessionId: string | null, content: unknown, status: string) {
  const threadRef = useRef<HTMLElement | null>(null);
  const contentRef = useRef<HTMLDivElement | null>(null);
  const followRef = useRef(true);
  const sessionRef = useRef(sessionId);
  const [showLatest, setShowLatest] = useState(false);

  const scrollToLatest = useCallback(() => {
    followRef.current = true;
    const thread = threadRef.current;
    if (thread) thread.scrollTop = thread.scrollHeight;
    setShowLatest(false);
  }, []);

  const onScroll = useCallback(() => {
    const thread = threadRef.current;
    if (!thread) return;
    const nearBottom = thread.scrollHeight - thread.scrollTop - thread.clientHeight < 80;
    followRef.current = nearBottom;
    setShowLatest(!nearBottom);
  }, []);

  useLayoutEffect(() => {
    if (sessionRef.current !== sessionId) {
      sessionRef.current = sessionId;
      followRef.current = true;
    }
    if (followRef.current) scrollToLatest();
  }, [sessionId, content, status, scrollToLatest]);

  useLayoutEffect(() => {
    const thread = threadRef.current;
    const body = contentRef.current;
    if (!thread || !body) return;
    // Markdown images, expanded references, and window resizing can change
    // height after React renders. Observe the content as well as the viewport.
    const observer = new ResizeObserver(() => {
      if (followRef.current) scrollToLatest();
    });
    observer.observe(thread);
    observer.observe(body);
    return () => observer.disconnect();
  }, [Boolean(content), sessionId, scrollToLatest]);

  return { threadRef, contentRef, onScroll, scrollToLatest, showLatest };
}
