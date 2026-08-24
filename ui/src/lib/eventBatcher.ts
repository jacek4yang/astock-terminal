export interface EventBatcher<T> {
  push(value: T): void;
  dispose(): void;
}

/**
 * Trailing-edge event coalescer shared by the real Agent stream and the
 * packaged performance harness. A single timer owns every queued event, so a
 * burst cannot schedule overlapping React updates.
 */
export function createEventBatcher<T>(
  intervalMs: number,
  onFlush: (batch: T[]) => void,
): EventBatcher<T> {
  if (!Number.isFinite(intervalMs) || intervalMs < 100) {
    throw new Error("Agent event batching interval must be at least 100 ms");
  }
  let queue: T[] = [];
  let timer: number | null = null;
  let disposed = false;

  const flush = () => {
    timer = null;
    if (disposed || !queue.length) return;
    const batch = queue;
    queue = [];
    onFlush(batch);
  };

  return {
    push(value) {
      if (disposed) return;
      queue.push(value);
      if (timer == null) timer = window.setTimeout(flush, intervalMs);
    },
    dispose() {
      disposed = true;
      queue = [];
      if (timer != null) window.clearTimeout(timer);
      timer = null;
    },
  };
}
