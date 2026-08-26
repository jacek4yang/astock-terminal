//! Slash commands must be aliases, never a second control plane.
//!
//! Every research-oriented shortcut has a natural-language equivalent, and this
//! suite is the enforcement mechanism. Each case asserts that the slash form
//! and the conversational form collapse to the *same* canonical
//! [`UserIntent`]. If someone later adds a slash-only handler, or lets one path
//! drift from the other, these tests fail.
//!
//! The suite also guards the opposite risk, which is the more damaging one: a
//! genuine research request must never be silently reinterpreted as a control
//! action just because it shares vocabulary with one.

use astock_agent_runtime::{ResearchDepth, UserIntent};

/// Assert that a slash alias and a conversational phrasing agree exactly.
#[track_caller]
fn assert_equivalent(slash: &str, natural: &str) {
    let from_slash = UserIntent::interpret(slash);
    let from_natural = UserIntent::interpret(natural);
    assert_eq!(
        from_slash, from_natural,
        "`{slash}` and `{natural}` must produce one canonical intent, got \
         {from_slash:?} and {from_natural:?}"
    );
}

#[test]
fn new_session_is_reachable_both_ways() {
    assert_equivalent("/new", "我们开一个新的研究会话");
    assert_eq!(UserIntent::interpret("/new"), UserIntent::NewSession);
}

#[test]
fn compact_is_reachable_both_ways() {
    assert_equivalent("/compact", "把这段长会话整理一下上下文，然后继续");
    assert_eq!(UserIntent::interpret("/compact"), UserIntent::Compact);
}

#[test]
fn plan_is_reachable_both_ways() {
    assert_equivalent("/plan", "给我看一下你现在准备怎么分析");
    assert_eq!(UserIntent::interpret("/plan"), UserIntent::ShowPlan);
}

#[test]
fn sources_is_reachable_both_ways() {
    assert_equivalent("/sources", "你目前用了哪些数据源？");
    assert_eq!(UserIntent::interpret("/sources"), UserIntent::ShowSources);
}

#[test]
fn evidence_is_reachable_both_ways() {
    assert_equivalent("/evidence", "把支持这个结论的证据列出来");
    assert_equivalent("/evidence", "这个结论依据是什么？");
    assert_eq!(UserIntent::interpret("/evidence"), UserIntent::ShowEvidence);
}

#[test]
fn cancel_is_reachable_both_ways() {
    assert_equivalent("/cancel", "停掉当前任务");
    assert_equivalent("/cancel", "先停一下");
    assert_eq!(UserIntent::interpret("/cancel"), UserIntent::Cancel);
}

#[test]
fn tools_is_reachable_both_ways() {
    assert_equivalent("/tools", "你有哪些工具？");
    assert_eq!(UserIntent::interpret("/tools"), UserIntent::ListTools);
}

#[test]
fn sessions_is_reachable_both_ways() {
    assert_equivalent("/sessions", "列出会话");
    assert_eq!(UserIntent::interpret("/sessions"), UserIntent::ListSessions);
}

#[test]
fn history_is_reachable_both_ways() {
    assert_equivalent("/history", "看一下会话历史");
    assert_eq!(UserIntent::interpret("/history"), UserIntent::ShowHistory);
}

#[test]
fn status_is_reachable_both_ways() {
    assert_equivalent("/status", "现在什么状态？");
    assert_eq!(UserIntent::interpret("/status"), UserIntent::ShowStatus);
}

#[test]
fn context_is_reachable_both_ways() {
    assert_equivalent("/context", "上下文用了多少？");
    assert_eq!(UserIntent::interpret("/context"), UserIntent::ShowContext);
}

#[test]
fn resume_without_an_identifier_is_reachable_both_ways() {
    assert_equivalent("/resume", "继续之前的会话");
    assert_eq!(
        UserIntent::interpret("/resume"),
        UserIntent::Resume { session_id: None }
    );
}

#[test]
fn resume_carries_the_same_identifier_from_either_form() {
    // The mission's worked example: `/resume abc` and `继续 abc 这个会话`.
    assert_equivalent("/resume abc123", "继续 abc123 这个会话");
    assert_eq!(
        UserIntent::interpret("/resume abc123"),
        UserIntent::Resume {
            session_id: Some("abc123".into())
        }
    );
    assert_equivalent("/resume 123", "继续会话 123");
}

#[test]
fn branch_is_reachable_both_ways() {
    assert_equivalent("/branch", "从刚才那个结论之前分一个新方向");
    assert_eq!(
        UserIntent::interpret("/branch"),
        UserIntent::Branch { message_id: None }
    );
}

#[test]
fn depth_is_reachable_both_ways_at_every_level() {
    assert_equivalent("/depth exhaustive", "这次给我做最深入的分析");
    assert_eq!(
        UserIntent::interpret("/depth exhaustive"),
        UserIntent::SetDepth {
            depth: ResearchDepth::Exhaustive
        }
    );
    assert_equivalent("/depth deep", "深入一点");
    assert_equivalent("/depth fast", "快速看一下");
}

#[test]
fn exit_is_reachable_both_ways() {
    assert_equivalent("/exit", "退出");
    assert_eq!(UserIntent::interpret("/exit"), UserIntent::Exit);
    // `/quit` is a spelling of the same intent, not a separate one.
    assert_eq!(UserIntent::interpret("/quit"), UserIntent::Exit);
}

#[test]
fn slash_aliases_collapse_onto_one_intent_each() {
    // Alternate spellings must not create additional semantics.
    assert_eq!(
        UserIntent::interpret("/stop"),
        UserIntent::interpret("/cancel")
    );
    assert_eq!(
        UserIntent::interpret("/todo"),
        UserIntent::interpret("/plan")
    );
    assert_eq!(
        UserIntent::interpret("/fork"),
        UserIntent::interpret("/branch")
    );
    assert_eq!(
        UserIntent::interpret("/cls"),
        UserIntent::interpret("/clear")
    );
}

#[test]
fn the_documented_shortcut_surface_is_covered_and_stays_compact() {
    // Section 4's recommended surface. Each must resolve to a real intent
    // rather than falling through to research, and the surface must not grow
    // into dozens of obscure commands.
    let surface = [
        "/new",
        "/resume",
        "/branch",
        "/sessions",
        "/history",
        "/compact",
        "/plan",
        "/depth deep",
        "/tools",
        "/sources",
        "/evidence",
        "/context",
        "/status",
        "/cancel",
        "/clear",
        "/help",
        "/exit",
    ];
    for command in surface {
        let intent = UserIntent::interpret(command);
        assert_ne!(
            intent.kind(),
            "research",
            "`{command}` is part of the documented surface and must resolve to a control intent"
        );
    }
    assert_eq!(
        surface.len(),
        17,
        "the shortcut surface changed; confirm the addition materially improves ergonomics \
         and has a natural-language equivalent before updating this count"
    );
}

#[test]
fn research_requests_are_never_captured_by_the_control_plane() {
    // These are the realistic user utterances from the product brief. Every
    // one must reach the Agent as research.
    for prompt in [
        "分析一下紫金矿业现在的投资价值",
        "我持有贵州茅台，成本 1480，现在应该重点关注什么风险？",
        "最近 AI 算力产业链哪些 A 股公司的基本面真的发生了变化？",
        "分析紫金矿业",
        "那如果铜价跌 15% 呢？",
        "帮我分析紫金矿业现在是否高估",
        "我更担心铜价，但是估值也一起看",
    ] {
        let intent = UserIntent::interpret(prompt);
        assert_eq!(
            intent.kind(),
            "research",
            "`{prompt}` must reach the Agent as research, got {intent:?}"
        );
    }
}

#[test]
fn only_screen_clearing_is_adapter_local_among_the_surface() {
    // Durable Agent truth must not be touched by a presentation concern, and
    // conversely a durable action must not be mistaken for a local one.
    assert!(UserIntent::interpret("/clear").is_adapter_local());
    for durable in ["/compact", "/new", "/branch", "/cancel", "/resume"] {
        assert!(
            !UserIntent::interpret(durable).is_adapter_local(),
            "`{durable}` changes durable state and must not be adapter-local"
        );
    }
}
