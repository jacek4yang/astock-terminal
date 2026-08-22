import { create } from "zustand";
import { persist } from "zustand/middleware";

export type UiMode = "pro" | "simple";
export type Theme = "dark" | "light";

interface AppState {
  /** 专业 / 小白模式 */
  mode: UiMode;
  /** 主题(深 / 浅,Tailwind dark class 策略) */
  theme: Theme;
  /** 当前选中个股代码(跨页记忆) */
  currentSymbol: string | null;
  /** 当前选中个股名称(用于 Agent 上下文展示) */
  currentName: string | null;
  setMode: (m: UiMode) => void;
  setTheme: (t: Theme) => void;
  setCurrentSymbol: (s: string | null) => void;
  setCurrentName: (n: string | null) => void;
}

export const useAppStore = create<AppState>()(
  persist(
    (set) => ({
      mode: "pro",
      theme: "dark",
      currentSymbol: null,
      currentName: null,
      setMode: (mode) => set({ mode }),
      setTheme: (theme) => set({ theme }),
      setCurrentSymbol: (currentSymbol) => set({ currentSymbol }),
      setCurrentName: (currentName) => set({ currentName }),
    }),
    { name: "astock-terminal-ui" },
  ),
);

/** 将主题应用到 <html> 的 dark class */
export function applyTheme(theme: Theme) {
  document.documentElement.classList.toggle("dark", theme === "dark");
}
