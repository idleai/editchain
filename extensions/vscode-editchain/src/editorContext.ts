import { EditorCapture } from './editorCapture';

/** Observe Git outside agent tool boundaries. Successful changes enter source order. */
export function observeEditorContext(capture: EditorCapture, request: () => Promise<any>, log: (message: string) => void): { dispose(): void } {
  let stopped = false;
  let busy = false;
  let previous = '';
  const poll = async () => {
    if (stopped || busy) return;
    busy = true;
    try {
      const response = await request();
      if (stopped) return;
      const context = response?.Ok;
      if (!Number.isSafeInteger(context?.observed_ms) || !Array.isArray(context?.repositories)) {
        throw new Error(JSON.stringify(response?.Error ?? response));
      }
      const key = JSON.stringify(context.repositories);
      if (key !== previous) { capture.context(context); previous = key; }
    } catch (error) {
      log(`Git context observation failed: ${String(error)}`);
      // End use of a previously observed anchor after an observation failure.
      if (!stopped && previous !== '[]') {
        capture.context({ observed_ms: Date.now(), repositories: [] }); previous = '[]';
      }
    } finally { busy = false; }
  };
  void poll();
  const timer = setInterval(() => { void poll(); }, 15000);
  timer.unref();
  return { dispose: () => { stopped = true; clearInterval(timer); } };
}
