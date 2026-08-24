import type { DockviewApi } from "dockview-react";
import type { WorkbenchPreset } from "./store";
import { panelTitles, type PanelId } from "./panelRegistry";

type Placement = {
  panel: PanelId;
  ref?: PanelId;
  direction?: "right" | "below" | "within";
  initialWidth?: number;
  initialHeight?: number;
};

const layouts: Record<WorkbenchPreset, Placement[]> = {
  market: [
    { panel: "watchlist", initialWidth: 238 },
    { panel: "market", ref: "watchlist", direction: "right", initialWidth: 900 },
    { panel: "marketList", ref: "market", direction: "below", initialHeight: 235 },
  ],
  research: [
    { panel: "security", initialWidth: 860 }, { panel: "market", ref: "security", direction: "right", initialWidth: 390 },
    { panel: "evidence", ref: "security", direction: "below", initialHeight: 230 },
  ],
  agent: [
    { panel: "agentTask" },
  ],
  quant: [
    { panel: "quant" }, { panel: "backtest", ref: "quant", direction: "right" },
    { panel: "security", ref: "quant", direction: "below" }, { panel: "diagnostics", ref: "security", direction: "within" },
  ],
  events: [
    { panel: "news" }, { panel: "disclosures", ref: "news", direction: "within" },
    { panel: "graph", ref: "news", direction: "right" }, { panel: "evidence", ref: "news", direction: "below" },
  ],
  settings: [
    { panel: "settings" },
  ],
};

export function applyPreset(api: DockviewApi, preset: WorkbenchPreset) {
  api.clear();
  for (const item of layouts[preset]) {
    api.addPanel({
      id: item.panel,
      component: item.panel,
      title: panelTitles[item.panel],
      initialWidth: item.initialWidth,
      initialHeight: item.initialHeight,
      ...(item.ref ? { position: { referencePanel: item.ref, direction: item.direction } } : {}),
    });
  }
}
