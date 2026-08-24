const AGENT_DRAFT_EVENT = "astock:agent-draft";

let pendingDraft: string | null = null;

/**
 * Transfers text into the v6 Agent composer without creating a second Agent
 * session store. This is presentation-only state: tasks, checkpoints and
 * history remain owned by Engine/Host.
 */
export function queueAgentDraft(prompt: string): boolean {
  const normalized = prompt.trim();
  if (!normalized) return false;
  pendingDraft = normalized;
  window.dispatchEvent(new CustomEvent(AGENT_DRAFT_EVENT, { detail: { prompt: normalized } }));
  return true;
}

export function consumeAgentDraft(): string | null {
  const draft = pendingDraft;
  pendingDraft = null;
  return draft;
}

export function subscribeAgentDraft(listener: (prompt: string) => void): () => void {
  const handler = (event: Event) => {
    const prompt = (event as CustomEvent<{ prompt?: unknown }>).detail?.prompt;
    if (typeof prompt === "string" && prompt.trim()) listener(prompt.trim());
  };
  window.addEventListener(AGENT_DRAFT_EVENT, handler);
  return () => window.removeEventListener(AGENT_DRAFT_EVENT, handler);
}
