import { useEffect, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { createEventBatcher } from "../lib/eventBatcher";

const LOGICAL_ROW_COUNT = 100_000;
const ROW_HEIGHT = 28;
const AGENT_BATCH_MS = 110;

/**
 * Packaged-renderer performance surface. It is unreachable from normal
 * navigation and activates only through the explicit release-test query used
 * together with ASTOCK_RELEASE_TEST_CDP. Measurements still exercise the
 * shipped React/CEF bundle and the same virtualizer/event coalescer as the
 * production surfaces; this component grants no bridge or data capability.
 */
export default function ReleasePerformanceHarness() {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const [agentBatches, setAgentBatches] = useState(0);
  const [agentEvents, setAgentEvents] = useState(0);
  const virtualizer = useVirtualizer({
    count: LOGICAL_ROW_COUNT,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 12,
  });
  const virtualItems = virtualizer.getVirtualItems();

  useEffect(() => {
    const batcher = createEventBatcher<number>(AGENT_BATCH_MS, (batch) => {
      setAgentBatches((value) => value + 1);
      setAgentEvents((value) => value + batch.length);
      window.dispatchEvent(new CustomEvent("astock:agent-render-batch", {
        detail: { at: performance.now(), event_count: batch.length },
      }));
    });
    const enqueue = (event: Event) => {
      const count = Math.max(1, Math.min(10_000, Number((event as CustomEvent).detail?.count) || 1));
      for (let index = 0; index < count; index += 1) batcher.push(index);
    };
    window.addEventListener("astock:release-agent-events", enqueue);
    return () => {
      window.removeEventListener("astock:release-agent-events", enqueue);
      batcher.dispose();
    };
  }, []);

  return <main
    className="release-performance-harness"
    data-release-performance-harness="v1"
    data-logical-rows={LOGICAL_ROW_COUNT}
    data-rendered-rows={virtualItems.length}
    data-agent-batches={agentBatches}
    data-agent-events={agentEvents}
  >
    <header>
      <strong>AStock Terminal · packaged performance fixture</strong>
      <span>100,000 logical rows · bounded DOM · 110 ms Agent batching</span>
    </header>
    <div ref={scrollRef} className="release-performance-scroll" aria-label="100k 逻辑行性能列表">
      <div style={{ height: virtualizer.getTotalSize(), position: "relative" }}>
        {virtualItems.map((row) => <div
          key={row.key}
          className="release-performance-row"
          data-row-index={row.index}
          style={{ height: row.size, transform: `translateY(${row.start}px)` }}
        >
          <span>{String(row.index + 1).padStart(6, "0")}</span>
          <b>确定性发布性能样本</b>
          <em>{(row.index * 2654435761 >>> 0).toString(16).padStart(8, "0")}</em>
        </div>)}
      </div>
    </div>
  </main>;
}
