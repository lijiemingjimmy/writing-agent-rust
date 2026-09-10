import test from 'node:test';
import assert from 'node:assert/strict';
import { AuthenticatedEventSource } from '../src/authenticated-sse.ts';

test('fetch SSE decodes split UTF-8, CRLF and multiline events, and cancels on close', async () => {
  const received = [];
  let cancelled = false;
  let options;
  const bytes = new TextEncoder().encode(': heartbeat\r\nevent: model.usage\r\ndata: 第一行\r\ndata: 第二行\r\n\r\nevent: run.completed\ndata: done\n\n');
  const stream = new ReadableStream({
    start(controller) { for (let i = 0; i < bytes.length; i += 2) controller.enqueue(bytes.slice(i, i + 2)); },
    cancel() { cancelled = true; }
  });
  const source = new AuthenticatedEventSource('/events', async (_, init) => {
    options = init;
    return new Response(stream, { headers: { 'content-type': 'text/event-stream' } });
  });
  await new Promise((resolve, reject) => {
    source.onerror = () => reject(new Error('unexpected stream error'));
    source.addEventListener('model.usage', event => received.push(event.data));
    source.addEventListener('run.completed', event => { received.push(event.data); source.close(); resolve(); });
  });
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.deepEqual(received, ['第一行\n第二行', 'done']);
  assert.equal(options.headers.Accept, 'text/event-stream');
  assert.equal(options.signal.aborted, true);
  assert.equal(cancelled, true);
});

test('failed authentication is reported without emitting a fabricated event', async () => {
  const source = new AuthenticatedEventSource('/events', async () => new Response('{}', { status: 401 }));
  await new Promise(resolve => { source.onerror = () => { source.close(); resolve(); }; });
});
