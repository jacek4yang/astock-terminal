import { create } from "zustand";
import { persist } from "zustand/middleware";

export type WorkbenchPreset = "market" | "agent" | "settings";

interface ResearchContextState {
  symbol: string | null;
  symbolName: string | null;
  asOf: string | null;
  range: string;
  benchmark: string | null;
  setSecurity: (symbol: string | null, name?: string | null) => void;
  setAsOf: (asOf: string | null) => void;
  setRange: (range: string) => void;
}

interface WorkspaceState {
  preset: WorkbenchPreset;
  marketChartHeight: number;
  setPreset: (preset: WorkbenchPreset) => void;
  setMarketChartHeight: (height: number) => void;
  resetLayout: () => void;
}

/** 研究上下文与界面布局分开持久化，切换工作区不会丢失当前证券。 */
export const useResearchContext = create<ResearchContextState>()(
  persist(
    (set) => ({
      symbol: null,
      symbolName: null,
      asOf: null,
      range: "1Y",
      benchmark: "000300",
      setSecurity: (symbol, symbolName = null) => set({ symbol, symbolName }),
      setAsOf: (asOf) => set({ asOf }),
      setRange: (range) => set({ range }),
    }),
    { name: "astock-research-context-v2" },
  ),
);

export const useWorkspaceStore = create<WorkspaceState>()(
  persist(
    (set) => ({
      preset: "agent",
      marketChartHeight: 540,
      setPreset: (preset) => set({ preset }),
      setMarketChartHeight: (marketChartHeight) => set({ marketChartHeight: Math.max(400, Math.min(820, marketChartHeight)) }),
      resetLayout: () => set({
        marketChartHeight: 540,
      }),
    }),
    { name: "astock-workspace-v6" },
  ),
);
