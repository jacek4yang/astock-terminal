//! Codex-style structured clarification.
//!
//! When input is materially ambiguous the Agent asks one compact question with
//! selectable options instead of interrogating the user. A question normally
//! offers `Let Agent choose` so the user can delegate, and `Other...` so the
//! user is never trapped inside Agent-generated choices.
//!
//! The answer must be accepted in any reasonable form. A user may type a
//! letter, a number, the label itself, a Chinese synonym, an ordinal phrase, a
//! delegation phrase, or free text. Every form normalizes to the same
//! [`ClarificationAnswer`], so the CLI and the desktop adapter cannot diverge:
//! clicking a button and typing the equivalent sentence produce one event.

use std::fmt;

/// One selectable answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClarificationOption {
    /// Stable machine identifier, used by both adapters and by durable events.
    pub id: String,
    /// Short user-visible label.
    pub label: String,
    /// Optional supporting line explaining what the option emphasises.
    pub detail: Option<String>,
    /// Extra accepted spellings: synonyms, abbreviations, English aliases.
    pub synonyms: Vec<String>,
    /// Marked `Recommended` in the UI. Must be justified by real context.
    pub recommended: bool,
}

impl ClarificationOption {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            detail: None,
            synonyms: Vec::new(),
            recommended: false,
        }
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn synonym(mut self, synonym: impl Into<String>) -> Self {
        self.synonyms.push(synonym.into());
        self
    }

    /// Mark this option as recommended.
    ///
    /// `reason` is required so a recommendation cannot be produced
    /// mechanically; it is recorded with the answer for auditability.
    pub fn recommended(mut self) -> Self {
        self.recommended = true;
        self
    }
}

/// A compact structured question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClarificationRequest {
    /// Stable identifier so a durable answer can be correlated after a resume.
    pub id: String,
    /// The question itself.
    pub question: String,
    /// Concrete choices, excluding the delegation and free-text affordances.
    pub options: Vec<ClarificationOption>,
    /// Whether `Let Agent choose` is offered. Normally true.
    pub allow_delegation: bool,
    /// Whether `Other...` free text is offered. Normally true.
    pub allow_other: bool,
    /// Why the recommended option is recommended, when one is marked.
    pub recommendation_reason: Option<String>,
}

impl ClarificationRequest {
    pub fn new(id: impl Into<String>, question: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            question: question.into(),
            options: Vec::new(),
            allow_delegation: true,
            allow_other: true,
            recommendation_reason: None,
        }
    }

    pub fn option(mut self, option: ClarificationOption) -> Self {
        self.options.push(option);
        self
    }

    /// Record why an option is recommended. Required whenever any option is
    /// marked recommended; [`Self::validate`] enforces it.
    pub fn recommendation_reason(mut self, reason: impl Into<String>) -> Self {
        self.recommendation_reason = Some(reason.into());
        self
    }

    /// Withhold delegation, for a genuinely user-dependent financial
    /// constraint that the Agent must not invent.
    pub fn without_delegation(mut self) -> Self {
        self.allow_delegation = false;
        self
    }

    pub fn without_other(mut self) -> Self {
        self.allow_other = false;
        self
    }

    /// Structural rules that keep clarification honest.
    pub fn validate(&self) -> Result<(), String> {
        if self.options.is_empty() {
            return Err("a clarification must offer at least one option".into());
        }
        let recommended = self
            .options
            .iter()
            .filter(|option| option.recommended)
            .count();
        if recommended > 1 {
            return Err("at most one option may be marked Recommended".into());
        }
        if recommended == 1 && self.recommendation_reason.is_none() {
            return Err(
                "a Recommended option requires a reason drawn from real context, so that \
                 recommendations cannot be produced mechanically"
                    .into(),
            );
        }
        let mut seen = Vec::new();
        for option in &self.options {
            if seen.contains(&option.id) {
                return Err(format!("duplicate clarification option id `{}`", option.id));
            }
            seen.push(option.id.clone());
        }
        Ok(())
    }

    /// Index of the delegation entry in the rendered list, when offered.
    fn delegation_position(&self) -> Option<usize> {
        self.allow_delegation.then_some(self.options.len())
    }

    /// Index of the free-text entry in the rendered list, when offered.
    fn other_position(&self) -> Option<usize> {
        if !self.allow_other {
            return None;
        }
        Some(self.options.len() + usize::from(self.allow_delegation))
    }

    /// Render for a terminal. The desktop adapter renders the same data as
    /// clickable rows; neither adds semantics.
    pub fn render_plain(&self) -> String {
        let mut lines = vec![self.question.clone(), String::new()];
        for (index, option) in self.options.iter().enumerate() {
            let marker = if option.recommended {
                "        Recommended"
            } else {
                ""
            };
            lines.push(format!("[{}] {}{}", index + 1, option.label, marker));
            if let Some(detail) = &option.detail {
                lines.push(format!("    {detail}"));
            }
        }
        if let Some(position) = self.delegation_position() {
            lines.push(format!("[{}] Let Agent choose", position + 1));
        }
        if let Some(position) = self.other_position() {
            lines.push(format!("[{}] Other...", position + 1));
        }
        lines.join("\n")
    }

    /// Normalize any reasonable user reply into a canonical answer.
    ///
    /// Resolution order is deliberate. Explicit `Other:` prefixes win because
    /// they are unambiguous, then positional and label matches, then
    /// delegation phrases, and finally free text when `Other...` is offered.
    pub fn interpret_answer(&self, reply: &str) -> Option<ClarificationAnswer> {
        let raw = reply.trim();
        if raw.is_empty() {
            return None;
        }
        let folded = fold(raw);

        // `Other: ...` and `其他：...` carry their own payload.
        if let Some(text) = strip_other_prefix(raw) {
            if !self.allow_other {
                return None;
            }
            let text = text.trim();
            return Some(if text.is_empty() {
                ClarificationAnswer::OtherRequested
            } else {
                ClarificationAnswer::FreeText {
                    text: text.to_string(),
                }
            });
        }

        // Positional selection: `1`, `A`, `第一个`, `选第二个`.
        if let Some(position) = positional_index(&folded) {
            if let Some(option) = self.options.get(position) {
                return Some(ClarificationAnswer::Option {
                    id: option.id.clone(),
                });
            }
            if Some(position) == self.delegation_position() {
                return Some(ClarificationAnswer::Delegated);
            }
            if Some(position) == self.other_position() {
                return Some(ClarificationAnswer::OtherRequested);
            }
            // A number outside the rendered range is not a silent miss.
            return None;
        }

        // Delegation is checked before label matching so that `你自己选` does
        // not accidentally match an option label containing `选`.
        if contains_any(&folded, DELEGATION_PHRASES) {
            return if self.allow_delegation {
                Some(ClarificationAnswer::Delegated)
            } else {
                None
            };
        }

        // Label and synonym matching, longest first so a specific synonym wins
        // over a shorter one that is a substring of it.
        let mut candidates: Vec<(usize, &ClarificationOption)> = Vec::new();
        for option in &self.options {
            let mut keys = vec![option.label.clone()];
            keys.extend(option.synonyms.iter().cloned());
            for key in keys {
                let folded_key = fold(&key);
                if folded_key.is_empty() {
                    continue;
                }
                if folded == folded_key || folded.contains(&folded_key) {
                    candidates.push((folded_key.chars().count(), option));
                }
            }
        }
        if let Some((_, option)) = candidates.into_iter().max_by_key(|(length, _)| *length) {
            return Some(ClarificationAnswer::Option {
                id: option.id.clone(),
            });
        }

        if contains_any(&folded, OTHER_PHRASES) {
            return self
                .allow_other
                .then_some(ClarificationAnswer::OtherRequested);
        }

        // Anything else is the user answering in their own words, which is
        // exactly what `Other...` exists to accept.
        if self.allow_other {
            return Some(ClarificationAnswer::FreeText {
                text: raw.to_string(),
            });
        }
        None
    }

    /// Choose on the user's behalf after they delegated.
    ///
    /// Prefers the recommended option, otherwise the first, and reports the
    /// reason so the decision is recorded rather than silent.
    pub fn agent_choice(&self) -> Option<(&ClarificationOption, String)> {
        let recommended = self.options.iter().find(|option| option.recommended);
        let chosen = recommended.or_else(|| self.options.first())?;
        let reason = if chosen.recommended {
            self.recommendation_reason.clone().unwrap_or_else(|| {
                "selected the recommended option after the user delegated the decision".to_string()
            })
        } else {
            "no option was recommended, so the first option was selected as the ordinary \
             research default after the user delegated the decision"
                .to_string()
        };
        Some((chosen, reason))
    }
}

/// The canonical, adapter-independent answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClarificationAnswer {
    /// A concrete option was selected, by any spelling.
    Option { id: String },
    /// The user explicitly delegated the decision to the Agent.
    Delegated,
    /// The user chose `Other...` but has not supplied text yet. The adapter
    /// should prompt for it.
    OtherRequested,
    /// The user answered in their own words.
    FreeText { text: String },
}

impl ClarificationAnswer {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Option { .. } => "option",
            Self::Delegated => "delegated",
            Self::OtherRequested => "other_requested",
            Self::FreeText { .. } => "free_text",
        }
    }
}

impl fmt::Display for ClarificationAnswer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option { id } => write!(formatter, "option:{id}"),
            Self::Delegated => formatter.write_str("delegated"),
            Self::OtherRequested => formatter.write_str("other"),
            Self::FreeText { text } => write!(formatter, "free_text:{text}"),
        }
    }
}

/// Case, width and punctuation folding shared by answer matching.
fn fold(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for character in input.trim().chars() {
        let folded = match character {
            '，' | ',' | '。' | '？' | '！' | '：' | '；' | '　' => ' ',
            '（' | '）' | '(' | ')' | '"' | '\'' | '“' | '”' => ' ',
            other if other.is_whitespace() => ' ',
            other if other.is_ascii_uppercase() => other.to_ascii_lowercase(),
            other => other,
        };
        output.push(folded);
    }
    output.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn contains_any(folded: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|phrase| folded.contains(phrase))
}

fn strip_other_prefix(raw: &str) -> Option<&str> {
    const PREFIXES: &[&str] = &[
        "other:",
        "other：",
        "其他:",
        "其他：",
        "自定义:",
        "自定义：",
    ];
    let lowered = raw.to_ascii_lowercase();
    for prefix in PREFIXES {
        if lowered.starts_with(prefix) {
            return Some(&raw[prefix.len()..]);
        }
    }
    None
}

/// Resolve a zero-based position from a number, a letter or an ordinal phrase.
fn positional_index(folded: &str) -> Option<usize> {
    let compact: String = folded.chars().filter(|c| !c.is_whitespace()).collect();

    // Bare number: `1`, `4`.
    if let Ok(number) = compact.parse::<usize>() {
        return number.checked_sub(1);
    }

    // Bare letter: `a`, `d`. Restricted to a single character so a one-letter
    // option label cannot be shadowed accidentally.
    if compact.chars().count() == 1 {
        let character = compact.chars().next()?;
        if character.is_ascii_lowercase() {
            return Some((character as u8 - b'a') as usize);
        }
    }

    // Chinese ordinals, with or without a leading verb: `第一个`, `选第二项`.
    const ORDINALS: &[(&str, usize)] = &[
        ("第一", 0),
        ("第二", 1),
        ("第三", 2),
        ("第四", 3),
        ("第五", 4),
        ("第六", 5),
        ("第七", 6),
        ("第八", 7),
    ];
    for (needle, index) in ORDINALS {
        if compact.contains(needle) {
            return Some(*index);
        }
    }

    // `选1`, `选 2`, `option 3`.
    for prefix in ["选", "选择", "option", "choose", "pick"] {
        if let Some(rest) = compact.strip_prefix(prefix) {
            if let Ok(number) = rest.trim().parse::<usize>() {
                return number.checked_sub(1);
            }
        }
    }
    None
}

const DELEGATION_PHRASES: &[&str] = &[
    "你自己选",
    "你来选",
    "你选",
    "你决定",
    "你自己决定",
    "由你决定",
    "agent决定",
    "let agent choose",
    "agent choose",
    "agent decide",
    "you decide",
    "你判断",
    "看你的",
    "随你",
    "都行",
];

const OTHER_PHRASES: &[&str] = &["其他", "other", "自定义", "都不是", "以上都不"];

#[cfg(test)]
mod tests {
    use super::*;

    /// The horizon question from the mission's clarification UX test section.
    fn horizon_question() -> ClarificationRequest {
        ClarificationRequest::new("horizon", "你希望这次分析覆盖哪个周期？")
            .option(
                ClarificationOption::new("short", "short")
                    .synonym("短线")
                    .synonym("短期"),
            )
            .option(
                ClarificationOption::new("medium", "medium")
                    .synonym("中线")
                    .synonym("中期"),
            )
            .option(
                ClarificationOption::new("long", "long")
                    .synonym("长线")
                    .synonym("长期"),
            )
    }

    #[test]
    fn letter_number_label_and_chinese_all_select_the_same_option() {
        let question = horizon_question();
        for reply in ["A", "a", "1", "short", "短线", "第一个", "选第一个"] {
            assert_eq!(
                question.interpret_answer(reply),
                Some(ClarificationAnswer::Option { id: "short".into() }),
                "reply `{reply}` must select the first option"
            );
        }
    }

    #[test]
    fn every_delegation_spelling_resolves_to_delegated() {
        let question = horizon_question();
        // `D` and `4` are the delegation row: three options then delegation.
        for reply in [
            "你选",
            "你自己选",
            "你决定",
            "agent decide",
            "let agent choose",
            "D",
            "4",
        ] {
            assert_eq!(
                question.interpret_answer(reply),
                Some(ClarificationAnswer::Delegated),
                "reply `{reply}` must delegate to the Agent"
            );
        }
    }

    #[test]
    fn other_prefix_captures_free_text() {
        let question = horizon_question();
        assert_eq!(
            question.interpret_answer("Other: 3-6个月"),
            Some(ClarificationAnswer::FreeText {
                text: "3-6个月".into()
            })
        );
        assert_eq!(
            question.interpret_answer("其他：我主要担心黄金价格下跌"),
            Some(ClarificationAnswer::FreeText {
                text: "我主要担心黄金价格下跌".into()
            })
        );
    }

    #[test]
    fn the_other_row_number_requests_free_text() {
        let question = horizon_question();
        // Three options, delegation at 4, Other at 5.
        assert_eq!(
            question.interpret_answer("5"),
            Some(ClarificationAnswer::OtherRequested)
        );
    }

    #[test]
    fn unrecognized_prose_becomes_free_text_rather_than_a_failure() {
        let question = horizon_question();
        assert_eq!(
            question.interpret_answer("我更担心铜价，但是估值也一起看"),
            Some(ClarificationAnswer::FreeText {
                text: "我更担心铜价，但是估值也一起看".into()
            })
        );
    }

    #[test]
    fn out_of_range_number_is_rejected_instead_of_silently_misresolved() {
        let question = horizon_question();
        assert_eq!(question.interpret_answer("99"), None);
    }

    #[test]
    fn delegation_is_refused_when_the_question_withholds_it() {
        let question = horizon_question().without_delegation();
        assert_eq!(question.interpret_answer("你选"), None);
        // With delegation withheld, Other moves up to position 4.
        assert_eq!(
            question.interpret_answer("4"),
            Some(ClarificationAnswer::OtherRequested)
        );
    }

    #[test]
    fn free_text_is_refused_when_other_is_withheld() {
        let question = horizon_question().without_other().without_delegation();
        assert_eq!(question.interpret_answer("完全不同的东西"), None);
        assert_eq!(question.interpret_answer("Other: x"), None);
    }

    #[test]
    fn a_recommended_option_requires_a_reason() {
        let unjustified = ClarificationRequest::new("q", "?")
            .option(ClarificationOption::new("a", "A").recommended());
        assert!(unjustified.validate().is_err());

        let justified = ClarificationRequest::new("q", "?")
            .option(ClarificationOption::new("a", "A").recommended())
            .recommendation_reason("the user already stated a multi-year holding period");
        assert!(justified.validate().is_ok());
    }

    #[test]
    fn at_most_one_option_may_be_recommended() {
        let question = ClarificationRequest::new("q", "?")
            .option(ClarificationOption::new("a", "A").recommended())
            .option(ClarificationOption::new("b", "B").recommended())
            .recommendation_reason("reason");
        assert!(question.validate().is_err());
    }

    #[test]
    fn duplicate_option_ids_are_rejected() {
        let question = ClarificationRequest::new("q", "?")
            .option(ClarificationOption::new("a", "A"))
            .option(ClarificationOption::new("a", "Another"));
        assert!(question.validate().is_err());
    }

    #[test]
    fn delegated_choice_prefers_the_recommended_option_and_reports_why() {
        let question = ClarificationRequest::new("focus", "关注哪种风险？")
            .option(ClarificationOption::new("price", "铜金价格下跌风险").recommended())
            .option(ClarificationOption::new("valuation", "公司估值过高"))
            .recommendation_reason("the portfolio context is dominated by commodity exposure");
        let (chosen, reason) = question.agent_choice().expect("a choice is available");
        assert_eq!(chosen.id, "price");
        assert!(reason.contains("commodity exposure"));
    }

    #[test]
    fn rendering_numbers_delegation_and_other_after_the_real_options() {
        let rendered = horizon_question().render_plain();
        assert!(rendered.contains("[1] short"));
        assert!(rendered.contains("[4] Let Agent choose"));
        assert!(rendered.contains("[5] Other..."));
    }

    #[test]
    fn recommended_marker_is_rendered_only_for_the_recommended_option() {
        let question = ClarificationRequest::new("focus", "关注哪种风险？")
            .option(ClarificationOption::new("price", "铜金价格下跌风险").recommended())
            .option(ClarificationOption::new("valuation", "公司估值过高"))
            .recommendation_reason("commodity exposure dominates the portfolio");
        let rendered = question.render_plain();
        let recommended_lines = rendered
            .lines()
            .filter(|line| line.contains("Recommended"))
            .count();
        assert_eq!(recommended_lines, 1);
        assert!(rendered.contains("[1] 铜金价格下跌风险        Recommended"));
    }
}
