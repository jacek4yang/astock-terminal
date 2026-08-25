import { useEffect } from "react";
import { HashRouter, Route, Routes } from "react-router-dom";
import { applyTheme, useAppStore } from "./store";
import WorkbenchShell from "./workbench/WorkbenchShell";

export default function App() {
  const theme = useAppStore((s) => s.theme);
  useEffect(() => {
    applyTheme(theme);
  }, [theme]);

  return (
    <HashRouter><Routes>
      <Route path="/stock/:symbol" element={<WorkbenchShell />} />
      <Route path="*" element={<WorkbenchShell />} />
    </Routes></HashRouter>
  );
}
