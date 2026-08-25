import { useEffect, useMemo, useState } from "react";
import { matchesCommand, type WorkbenchCommand } from "./commands";

export default function CommandPalette({ open, commands, onClose }: { open: boolean; commands: WorkbenchCommand[]; onClose: () => void }) {
  const [query, setQuery] = useState("");
  const filtered = useMemo(() => commands.filter((command) => matchesCommand(command, query)), [commands, query]);

  useEffect(() => {
    if (open) setQuery("");
  }, [open]);
  if (!open) return null;

  return <div className="command-backdrop" role="presentation" onMouseDown={onClose}>
    <div className="command-palette" role="dialog" aria-modal="true" aria-label="命令面板" onMouseDown={(event) => event.stopPropagation()}>
      <div className="command-input"><span>›</span><input autoFocus value={query} onChange={(event) => setQuery(event.target.value)} onKeyDown={(event) => { if (event.key === "Escape") onClose(); if (event.key === "Enter" && filtered[0]) { filtered[0].run(); onClose(); } }} placeholder="输入命令、面板或工作区…" /><kbd>Esc</kbd></div>
      <div className="command-results">{filtered.map((command) => <button key={command.id} onClick={() => { command.run(); onClose(); }}><span><small>{command.category}</small>{command.title}</span>{command.shortcut && <kbd>{command.shortcut}</kbd>}</button>)}{!filtered.length && <div className="empty-command">没有匹配命令</div>}</div>
    </div>
  </div>;
}
