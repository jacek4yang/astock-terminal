import { create } from "zustand";
import { persist } from "zustand/middleware";

export type WorkbenchPreset = "market" | "research" | "agent" | "quant" | "events" | "settings";
export type InspectorTab = "context" | "selection" | "source";

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
  inspectorOpen: boolean;
  inspectorTab: InspectorTab;
  inspectorWidth: number;
  marketSidebarWidth: number;
  marketTableHeight: number;
  marketChartHeight: number;
  bottomOpen: boolean;
  bottomHeight: number;
  savedLayout: unknown | null;
  layoutRevision: number;
  setPreset: (preset: WorkbenchPreset) => void;
  setInspectorOpen: (open: boolean) => void;
  setInspectorTab: (tab: InspectorTab) => void;
  setInspectorWidth: (width: number) => void;
  setMarketSidebarWidth: (width: number) => void;
  setMarketTableHeight: (height: number) => void;
  setMarketChartHeight: (height: number) => void;
  setBottomOpen: (open: boolean) => void;
  setBottomHeight: (height: number) => void;
  saveLayout: (layout: unknown) => void;
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
      inspectorOpen: true,
      inspectorTab: "context",
      inspectorWidth: 228,
      marketSidebarWidth: 256,
      marketTableHeight: 280,
      marketChartHeight: 540,
      bottomOpen: false,
      bottomHeight: 118,
      savedLayout: null,
      layoutRevision: 0,
      setPreset: (preset) => set({ preset, savedLayout: null, layoutRevision: Date.now() }),
      setInspectorOpen: (inspectorOpen) => set({ inspectorOpen }),
      setInspectorTab: (inspectorTab) => set({ inspectorTab }),
      setInspectorWidth: (inspectorWidth) => set({ inspectorWidth: Math.max(190, Math.min(420, inspectorWidth)) }),
      setMarketSidebarWidth: (marketSidebarWidth) => set({ marketSidebarWidth: Math.max(210, Math.min(420, marketSidebarWidth)) }),
      setMarketTableHeight: (marketTableHeight) => set({ marketTableHeight: Math.max(180, Math.min(520, marketTableHeight)) }),
      setMarketChartHeight: (marketChartHeight) => set({ marketChartHeight: Math.max(400, Math.min(820, marketChartHeight)) }),
      setBottomOpen: (bottomOpen) => set({ bottomOpen }),
      setBottomHeight: (bottomHeight) => set({ bottomHeight: Math.max(82, Math.min(360, bottomHeight)) }),
      saveLayout: (savedLayout) => set({ savedLayout }),
      resetLayout: () => set({
        savedLayout: null,
        marketChartHeight: 540,
        layoutRevision: Date.now(),
      }),
    }),
    { name: "astock-workspace-v5" },
  ),
);
