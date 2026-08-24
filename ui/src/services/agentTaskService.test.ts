import { beforeEach, describe, expect, it, vi } from "vitest";

const { requestNative } = vi.hoisted(() => ({ requestNative: vi.fn() }));
vi.mock("../bridge", () => ({ requestNative }));

import { agentTaskService, publicAgentTaskMethods } from "./agentTaskService";

describe("stable Agent task service facade", () => {
  beforeEach(() => requestNative.mockReset().mockResolvedValue({ ok: true }));

  it("declares every v6 public task method", () => {
    expect([...publicAgentTaskMethods]).toEqual([
      "task.create", "task.list", "task.get", "task.branch", "task.resume", "task.cancel", "task.answer",
    ]);
  });

  it("keeps create, answer and cancel behind Host-owned durable Agent calls", async () => {
    await agentTaskService.create({ task_id: "task-1", seq: 1, spec: {} });
    await agentTaskService.answer({ task_id: "task-1", seq: 2, clarification_response: {} });
    await agentTaskService.cancel("task-1", 2);

    expect(requestNative.mock.calls).toEqual([
      ["agent", "agent.start", { task_id: "task-1", seq: 1, spec: {} }, { deadlineMs: 120_000 }],
      ["agent", "agent.event", { task_id: "task-1", seq: 2, clarification_response: {}, event_kind: "clarification_answered" }, { deadlineMs: 120_000 }],
      ["agent", "agent.event", { task_id: "task-1", seq: 3, event_kind: "cancel" }, { deadlineMs: 120_000 }],
    ]);
  });

  it("exposes only bounded durable reads and branch operations to history callers", async () => {
    await agentTaskService.list(80, "茅台");
    await agentTaskService.get("task-1");
    await agentTaskService.branch({
      source_conversation_id: "c1",
      new_conversation_id: "c2",
      message_id: "m1",
      title: "分支",
      checkpoint_task_id: "task-1",
      checkpoint_accepted_seq: 4,
    });
    expect(requestNative.mock.calls.map((call) => call.slice(0, 2))).toEqual([
      ["engine", "agent.conversation.list"],
      ["engine", "agent.task.load"],
      ["engine", "agent.conversation.branch"],
    ]);
    expect(requestNative.mock.calls[2][2]).toMatchObject({
      checkpoint_task_id: "task-1",
      checkpoint_accepted_seq: 4,
    });
  });
});
