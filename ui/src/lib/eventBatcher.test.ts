import { afterEach, describe, expect, it, vi } from "vitest";
import { createEventBatcher } from "./eventBatcher";

describe("Agent event batching", () => {
  afterEach(() => vi.useRealTimers());

  it("coalesces bursts into no more than ten flushes per second", () => {
    vi.useFakeTimers();
    const batches: number[][] = [];
    const batcher = createEventBatcher<number>(110, (batch) => batches.push(batch));
    for (let index = 0; index < 100; index += 1) {
      batcher.push(index);
      vi.advanceTimersByTime(10);
    }
    vi.advanceTimersByTime(200);
    expect(batches.length).toBeLessThanOrEqual(10);
    expect(batches.flat()).toEqual(Array.from({ length: 100 }, (_, index) => index));
    batcher.dispose();
  });

  it("drops queued work after disposal", () => {
    vi.useFakeTimers();
    const flush = vi.fn();
    const batcher = createEventBatcher(110, flush);
    batcher.push(1);
    batcher.dispose();
    vi.advanceTimersByTime(500);
    expect(flush).not.toHaveBeenCalled();
  });
});
