//! Canonical user intent.
//!
//! Natural language is AStock's primary interface. Slash commands exist only
//! as convenience aliases for experienced users, so both inputs are resolved
//! here into the same [`UserIntent`], and the runtime acts on nothing else.
//! Writing a second slash-only handler is forbidden: the two paths would drift
//! and the product would degrade into a CLI that happens to embed a model.
//!
//! ```text
//! slash input ──┐
//!               ├──> interpret ──> UserIntent ──> Agent Runtime
//! natural text ─┘
//! ```
//!
//! The natural-language rules are deliberately conservative. A research
//! request must never be swallowed by a control keyword, so every
//! conversational rule is guarded: it only fires for a short utterance that
//! carries no research subject. When in doubt the input becomes
//! [`UserIntent::Research`], which is the safe default because the Agent then
//! reasons about it instead of silently performing a control action.

use std::fmt;

/// How much research effort the user wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResearchDepth {
    Fast,
    Balanced,
    Deep,
    Exhaustive,
}

impl ResearchDepth {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Balanced => "balanced",
            Self::Deep => "deep",
            Self::Exhaustive => "exhaustive",
        }
    }

    /// Parse a depth token from configuration, a slash argument or a phrase.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "fast" | "quick" => Some(Self::Fast),
            "balanced" | "normal" | "default" => Some(Self::Balanced),
            "deep" => Some(Self::Deep),
            "exhaustive" | "max" | "maximum" => Some(Self::Exhaustive),
            _ => None,
        }
    }
}

impl fmt::Display for ResearchDepth {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// The single canonical description of what the user asked for.
///
/// Both `/resume abc` and `继续 abc 这个会话` produce
/// `Resume { session_id: Some("abc") }`. Adapters may render intents
/// differently but must not interpret them differently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserIntent {
    /// An ordinary research request. This is the default and by far the most
    /// common intent; the Agent decides the workflow from conversation.
    Research { prompt: String },
    /// Start a fresh durable conversation.
    NewSession,
    /// Continue the latest or an identified durable conversation.
    Resume { session_id: Option<String> },
    /// Fork a conversation at its latest or an identified message.
    Branch { message_id: Option<String> },
    /// List durable conversations.
    ListSessions,
    /// Show the conversation transcript.
    ShowHistory,
    /// Refresh the bounded model-context index without deleting history.
    Compact,
    /// Show the current user-visible research plan.
    ShowPlan,
    /// Change research depth for subsequent work.
    SetDepth { depth: ResearchDepth },
    /// List the bounded typed tools the Agent may call.
    ListTools,
    /// Show which data sources have been consulted.
    ShowSources,
    /// Show local cache and storage counters.
    ///
    /// Not part of the documented shortcut surface, but retained because the
    /// interactive adapter already offered it and `astock cache` exists as a
    /// top-level command; removing it would be an unjustified regression.
    ShowCache,
    /// Show evidence backing the current conclusions.
    ShowEvidence,
    /// Show context/window accounting.
    ShowContext,
    /// Show durable task status.
    ShowStatus,
    /// Cooperatively cancel the running durable task.
    Cancel,
    /// Show the shortcut surface.
    Help,
    /// Leave the adapter.
    Exit,
    /// Clear the visible screen. Adapter-local: it must never affect durable
    /// Agent truth, which is why it is distinguished from `Compact`.
    ClearScreen,
}

impl UserIntent {
    /// Stable discriminant, useful for events, logs and equivalence tests.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Research { .. } => "research",
            Self::NewSession => "new_session",
            Self::Resume { .. } => "resume",
            Self::Branch { .. } => "branch",
            Self::ListSessions => "list_sessions",
            Self::ShowHistory => "show_history",
            Self::Compact => "compact",
            Self::ShowPlan => "show_plan",
            Self::SetDepth { .. } => "set_depth",
            Self::ListTools => "list_tools",
            Self::ShowSources => "show_sources",
            Self::ShowCache => "show_cache",
            Self::ShowEvidence => "show_evidence",
            Self::ShowContext => "show_context",
            Self::ShowStatus => "show_status",
            Self::Cancel => "cancel",
            Self::Help => "help",
            Self::Exit => "exit",
            Self::ClearScreen => "clear_screen",
        }
    }

    /// True when the intent is a pure adapter concern that must not reach
    /// durable Agent state.
    pub fn is_adapter_local(&self) -> bool {
        matches!(self, Self::ClearScreen | Self::Help | Self::Exit)
    }

    /// Resolve any user input into canonical intent.
    ///
    /// Slash input is tried first because it is unambiguous. Everything else
    /// goes through conservative conversational rules and otherwise becomes a
    /// research request.
    pub fn interpret(input: &str) -> Self {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return Self::Research {
                prompt: String::new(),
            };
        }
        if let Some(intent) = Self::from_slash(trimmed) {
            return intent;
        }
        if let Some(intent) = Self::from_natural_language(trimmed) {
            return intent;
        }
        Self::Research {
            prompt: trimmed.to_string(),
        }
    }

    /// Parse an explicit slash alias. Returns `None` when the input is not a
    /// slash command, and `Some(Research)` for an unknown slash token so that
    /// a typo is answered conversationally instead of silently ignored.
    pub fn from_slash(input: &str) -> Option<Self> {
        let trimmed = input.trim();
        let rest = trimmed.strip_prefix('/')?;
        let (command, argument) = match rest.split_once(char::is_whitespace) {
            Some((command, argument)) => (command, argument.trim()),
            None => (rest, ""),
        };
        let optional = |value: &str| (!value.is_empty()).then(|| value.to_string());

        let intent = match command.to_ascii_lowercase().as_str() {
            "new" => Self::NewSession,
            "resume" | "continue" => Self::Resume {
                session_id: optional(argument),
            },
            "branch" | "fork" => Self::Branch {
                message_id: optional(argument),
            },
            "sessions" => Self::ListSessions,
            "history" => Self::ShowHistory,
            "compact" => Self::Compact,
            "plan" | "todo" => Self::ShowPlan,
            "depth" => match ResearchDepth::parse(argument) {
                Some(depth) => Self::SetDepth { depth },
                // An unusable depth argument is a question, not a state
                // change, so it is answered rather than applied.
                None => Self::Research {
                    prompt: trimmed.to_string(),
                },
            },
            "tools" => Self::ListTools,
            "sources" => Self::ShowSources,
            "cache" => Self::ShowCache,
            "evidence" => Self::ShowEvidence,
            "context" => Self::ShowContext,
            "status" => Self::ShowStatus,
            "cancel" | "stop" | "abort" => Self::Cancel,
            "help" | "?" => Self::Help,
            "exit" | "quit" => Self::Exit,
            "clear" | "cls" => Self::ClearScreen,
            _ => Self::Research {
                prompt: trimmed.to_string(),
            },
        };
        Some(intent)
    }

    /// Conservative conversational rules.
    ///
    /// Every rule is guarded by [`is_probably_control_utterance`], so a real
    /// research question keeps its meaning even when it happens to share a
    /// word with a control phrase.
    fn from_natural_language(input: &str) -> Option<Self> {
        let normalized = normalize(input);
        if !is_probably_control_utterance(&normalized) {
            return None;
        }

        // Resume is checked before the generic "continue" phrasing so that an
        // explicit session reference keeps its identifier.
        if contains_any(&normalized, RESUME_PHRASES) {
            return Some(Self::Resume {
                session_id: extract_identifier(input),
            });
        }
        if contains_any(&normalized, BRANCH_PHRASES) {
            return Some(Self::Branch {
                message_id: extract_identifier(input),
            });
        }
        if contains_any(&normalized, NEW_SESSION_PHRASES) {
            return Some(Self::NewSession);
        }
        if contains_any(&normalized, CANCEL_PHRASES) {
            return Some(Self::Cancel);
        }
        if contains_any(&normalized, COMPACT_PHRASES) {
            return Some(Self::Compact);
        }
        if contains_any(&normalized, PLAN_PHRASES) {
            return Some(Self::ShowPlan);
        }
        if contains_any(&normalized, EVIDENCE_PHRASES) {
            return Some(Self::ShowEvidence);
        }
        if contains_any(&normalized, SOURCES_PHRASES) {
            return Some(Self::ShowSources);
        }
        if contains_any(&normalized, CACHE_PHRASES) {
            return Some(Self::ShowCache);
        }
        if contains_any(&normalized, TOOLS_PHRASES) {
            return Some(Self::ListTools);
        }
        if contains_any(&normalized, SESSIONS_PHRASES) {
            return Some(Self::ListSessions);
        }
        if contains_any(&normalized, HISTORY_PHRASES) {
            return Some(Self::ShowHistory);
        }
        if contains_any(&normalized, CONTEXT_PHRASES) {
            return Some(Self::ShowContext);
        }
        if contains_any(&normalized, STATUS_PHRASES) {
            return Some(Self::ShowStatus);
        }
        if let Some(depth) = depth_from_phrase(&normalized) {
            // A depth directive must not carry a domain topic; `深入一点看看
            // 这家公司的现金流` is research, not a settings change.
            if !mentions_research_topic(&normalized) {
                return Some(Self::SetDepth { depth });
            }
        }
        if contains_any(&normalized, EXIT_PHRASES) {
            return Some(Self::Exit);
        }
        None
    }
}

/// Longest utterance still eligible for a conversational control rule.
///
/// Real research requests in this product are materially longer than a control
/// phrase such as `先停一下`. The bound keeps a long analytical question from
/// ever being reinterpreted as a control action.
const MAX_CONTROL_UTTERANCE_CHARS: usize = 44;

/// Reject control interpretation when the utterance looks like research.
///
/// Two signals disqualify it: excessive length, and the presence of a security
/// identifier such as `601899`, which only appears when the user is talking
/// about an instrument rather than about the session.
fn is_probably_control_utterance(normalized: &str) -> bool {
    if normalized.chars().count() > MAX_CONTROL_UTTERANCE_CHARS {
        return false;
    }
    !contains_security_identifier(normalized)
}

/// True when the text contains a run of six or more digits, the shape of an
/// A-share code. Session identifiers are matched separately and are not
/// six-digit decimal runs in practice.
fn contains_security_identifier(text: &str) -> bool {
    let mut run = 0usize;
    for character in text.chars() {
        if character.is_ascii_digit() {
            run += 1;
            if run >= 6 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// Fold width, case and punctuation so that phrase matching is stable across
/// full-width Chinese input and ASCII input.
fn normalize(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for character in input.trim().chars() {
        let folded = match character {
            // Full-width forms commonly produced by Chinese IMEs.
            '，' | ',' => ' ',
            '。' | '．' => ' ',
            '？' => ' ',
            '！' => ' ',
            '：' | '；' => ' ',
            '（' | '）' | '(' | ')' => ' ',
            '“' | '”' | '‘' | '’' | '"' | '\'' => ' ',
            '　' => ' ',
            other if other.is_whitespace() => ' ',
            other if other.is_ascii_uppercase() => other.to_ascii_lowercase(),
            other => other,
        };
        output.push(folded);
    }
    // Collapse runs of spaces so phrase tables need no whitespace variants.
    let mut collapsed = String::with_capacity(output.len());
    let mut previous_space = false;
    for character in output.chars() {
        let is_space = character == ' ';
        if !(is_space && previous_space) {
            collapsed.push(character);
        }
        previous_space = is_space;
    }
    collapsed.trim().to_string()
}

fn contains_any(normalized: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|phrase| normalized.contains(phrase))
}

/// Recognize a depth directive that carries no research subject.
///
/// The phrase tables deliberately hold complete directives rather than bare
/// adjectives. An earlier version matched the bare word `深入`, which
/// misclassified the genuine research request
/// `继续深入研究这家公司的现金流质量...` as a depth change. A depth directive is
/// a statement about *how* to work, so it must look like one; when in doubt the
/// input stays a research request and the Agent infers depth from context.
///
/// Order matters: the most specific superlative is tested first so that
/// `最深入的分析` is not consumed by a shorter rule.
fn depth_from_phrase(normalized: &str) -> Option<ResearchDepth> {
    const EXHAUSTIVE: &[&str] = &[
        "最深入的分析",
        "最详细的分析",
        "最全面的分析",
        "最深入的研究",
        "尽可能深入",
        "尽可能详细",
        "彻底分析一下",
        "exhaustive",
    ];
    const DEEP: &[&str] = &[
        "深入一点",
        "深入一些",
        "再深入点",
        "详细一点",
        "详细一些",
        "更详细一点",
        "分析深一点",
        "deep",
    ];
    const FAST: &[&str] = &[
        "快速看一下",
        "快速看看",
        "简单看一下",
        "简单看看",
        "大概看一下",
        "粗略看一下",
        "快点看",
        "fast",
        "quick",
    ];
    const BALANCED: &[&str] = &["正常深度", "平衡一点", "balanced"];

    // A depth directive is short. Anything longer is a research request that
    // happens to mention effort.
    const MAX_DEPTH_DIRECTIVE_CHARS: usize = 20;
    if normalized.chars().count() > MAX_DEPTH_DIRECTIVE_CHARS {
        return None;
    }

    if contains_any(normalized, EXHAUSTIVE) {
        return Some(ResearchDepth::Exhaustive);
    }
    if contains_any(normalized, DEEP) {
        return Some(ResearchDepth::Deep);
    }
    if contains_any(normalized, FAST) {
        return Some(ResearchDepth::Fast);
    }
    if contains_any(normalized, BALANCED) {
        return Some(ResearchDepth::Balanced);
    }
    None
}

/// Vocabulary that only appears when the user is discussing an instrument,
/// company, sector or market rather than the session itself.
///
/// Used as defence in depth: even a short utterance is treated as research when
/// it carries a domain topic, because misreading research as a control action
/// is the more damaging failure.
const RESEARCH_TOPIC_MARKERS: &[&str] = &[
    "公司",
    "股票",
    "个股",
    "行业",
    "板块",
    "估值",
    "财报",
    "年报",
    "季报",
    "现金流",
    "基本面",
    "走势",
    "业绩",
    "利润",
    "营收",
    "毛利",
    "股价",
    "仓位",
    "持仓",
    "市场",
    "指数",
    "产业链",
];

fn mentions_research_topic(normalized: &str) -> bool {
    contains_any(normalized, RESEARCH_TOPIC_MARKERS)
}

/// Pull a session or message identifier out of a conversational reference.
///
/// Only tokens that look like machine identifiers qualify, so ordinary Chinese
/// words are never mistaken for an identifier. A short pure-digit token is
/// accepted because users do refer to `会话 123`.
fn extract_identifier(input: &str) -> Option<String> {
    input
        .split(|character: char| {
            character.is_whitespace()
                || matches!(
                    character,
                    '，' | ',' | '。' | '？' | '！' | '：' | '；' | '(' | ')' | '（' | '）'
                )
        })
        .find(|token| is_identifier_token(token))
        .map(|token| token.to_string())
}

fn is_identifier_token(token: &str) -> bool {
    if token.len() < 2 || token.len() > 64 {
        return false;
    }
    // Must be machine-shaped: ASCII alphanumeric plus separators only.
    if !token
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return false;
    }
    if !token.chars().any(|character| character.is_ascii_digit()) {
        // Reject English control words that are otherwise identifier-shaped.
        return false;
    }
    true
}

const RESUME_PHRASES: &[&str] = &[
    "继续会话",
    "继续之前的会话",
    "继续上次",
    "接着上次",
    "接着之前",
    "恢复会话",
    "回到会话",
    "这个会话",
    "那个会话",
    "resume",
];

const BRANCH_PHRASES: &[&str] = &[
    "分一个新方向",
    "分支",
    "另开一个方向",
    "换个方向继续",
    "从刚才",
    "从那个结论",
    "branch",
];

const NEW_SESSION_PHRASES: &[&str] = &[
    "新的研究会话",
    "开一个新会话",
    "新建会话",
    "重新开始",
    "开个新的",
    "换个新会话",
];

const CANCEL_PHRASES: &[&str] = &[
    "先停一下",
    "停一下",
    "停掉",
    "停止",
    "取消当前",
    "取消任务",
    "别做了",
    "算了不用了",
    "中断",
];

const COMPACT_PHRASES: &[&str] = &[
    "整理一下上下文",
    "整理上下文",
    "压缩上下文",
    "精简上下文",
    "上下文整理",
    "整理一下这段会话",
    "整理一下对话",
];

const PLAN_PHRASES: &[&str] = &[
    "准备怎么分析",
    "打算怎么分析",
    "你的计划",
    "研究计划",
    "看一下计划",
    "现在的计划",
    "接下来做什么",
    "下一步做什么",
];

const EVIDENCE_PHRASES: &[&str] = &[
    "证据列出来",
    "列出证据",
    "有哪些证据",
    "依据是什么",
    "凭什么这么说",
    "支持这个结论",
    "结论的依据",
];

const SOURCES_PHRASES: &[&str] = &[
    "哪些数据源",
    "什么数据源",
    "数据来源有哪些",
    "用了哪些来源",
    "数据源列表",
];

const CACHE_PHRASES: &[&str] = &["缓存用了多少", "缓存情况", "缓存大小", "本地缓存"];

const TOOLS_PHRASES: &[&str] = &["哪些工具", "什么工具", "工具列表", "你会用什么"];

const SESSIONS_PHRASES: &[&str] = &["有哪些会话", "列出会话", "会话列表", "我的会话", "所有会话"];

const HISTORY_PHRASES: &[&str] = &[
    "对话历史",
    "会话历史",
    "历史记录",
    "之前说了什么",
    "聊了什么",
];

const CONTEXT_PHRASES: &[&str] = &[
    "上下文用了多少",
    "上下文情况",
    "上下文大小",
    "还有多少上下文",
];

const STATUS_PHRASES: &[&str] = &[
    "现在什么状态",
    "当前状态",
    "任务状态",
    "进展如何",
    "做到哪了",
];

const EXIT_PHRASES: &[&str] = &["退出", "再见", "结束吧", "不聊了"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_research_is_never_hijacked_by_a_control_keyword() {
        // Each of these shares vocabulary with a control phrase but is a
        // genuine research request and must stay one.
        for prompt in [
            "分析一下紫金矿业现在的投资价值",
            "我持有贵州茅台，成本 1480，现在应该重点关注什么风险？",
            "最近 AI 算力产业链哪些 A 股公司的基本面真的发生了变化？",
            "深入分析601899的铜价敏感性",
            "继续深入研究这家公司的现金流质量和长期竞争壮况如何变化",
            "把紫金矿业和洛阳钼业的证据放在一起对比一下基本面差异",
        ] {
            let intent = UserIntent::interpret(prompt);
            assert_eq!(
                intent.kind(),
                "research",
                "`{prompt}` must remain a research request, got {intent:?}"
            );
        }
    }

    #[test]
    fn empty_input_is_an_empty_research_request_not_a_control_action() {
        assert_eq!(
            UserIntent::interpret("   "),
            UserIntent::Research {
                prompt: String::new()
            }
        );
    }

    #[test]
    fn unknown_slash_token_is_answered_conversationally() {
        let intent = UserIntent::interpret("/definitelynotacommand");
        assert_eq!(
            intent,
            UserIntent::Research {
                prompt: "/definitelynotacommand".to_string()
            }
        );
    }

    #[test]
    fn depth_slash_with_an_unusable_argument_does_not_change_state() {
        let intent = UserIntent::interpret("/depth sideways");
        assert_eq!(intent.kind(), "research");
    }

    #[test]
    fn clear_screen_stays_adapter_local_and_compact_does_not() {
        assert!(UserIntent::ClearScreen.is_adapter_local());
        assert!(!UserIntent::Compact.is_adapter_local());
    }

    #[test]
    fn security_identifier_guard_detects_a_share_codes() {
        assert!(contains_security_identifier("601899"));
        assert!(contains_security_identifier("看一下 000001 的走势"));
        assert!(!contains_security_identifier("会话 123"));
    }

    #[test]
    fn normalization_folds_full_width_punctuation_and_case() {
        assert_eq!(normalize("你目前用了哪些数据源？"), "你目前用了哪些数据源");
        assert_eq!(normalize("  Resume   ABC  "), "resume abc");
    }

    #[test]
    fn identifier_extraction_ignores_ordinary_words() {
        assert_eq!(
            extract_identifier("继续 abc123 这个会话"),
            Some("abc123".into())
        );
        assert_eq!(extract_identifier("继续会话 123"), Some("123".into()));
        assert_eq!(extract_identifier("继续之前的会话"), None);
    }
}
