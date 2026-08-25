export interface WorkbenchCommand {
  id: string;
  title: string;
  category: string;
  shortcut?: string;
  keywords?: string[];
  run: () => void;
}

export function matchesCommand(command: WorkbenchCommand, query: string): boolean {
  const haystack = [command.title, command.category, command.id, ...(command.keywords ?? [])].join(" ").toLowerCase();
  return query.trim().split(/\s+/).every((term) => haystack.includes(term.toLowerCase()));
}
