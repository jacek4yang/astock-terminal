//! The model control plane.
//!
//! Language architecture: this prompt, the tool descriptions and every repair
//! diagnostic are **English**, because they are machine control surface. All
//! user-visible output is **Simplified Chinese** by default, carried explicitly as
//! `output_language` rather than left for the model to infer. Source evidence keeps
//! its original language — Chinese disclosures stay Chinese, and nothing is
//! translated merely to make the control plane uniform.
//!
//! The prompt deliberately does *not* restate what the type system already
//! guarantees. Evidence identifier shape, claim/provenance compatibility, bounds
//! and citation formatting are enforced by the `submit_report` schema, by
//! `ClaimKind::permits` and by the renderer, so spending prompt tokens on them buys
//! nothing and drifts out of date. What remains is the set of rules that genuinely
//! need the model's cooperation: which evidence to seek, which claim kind an
//! assertion really is, when to compute rather than assert, and what to disclose.
//!
//! It also does not instruct the model to reason in any particular language or to
//! show its reasoning. Reasoning depth is a provider-level concern driven by task
//! depth; private reasoning stays private.
//!
//! Layout is chosen for prompt caching: the static text below is byte-stable across
//! rounds and tasks, and only a short `TASK` block varies.

use crate::runtime::RuntimeTask;

/// Stable control-plane prompt. Byte-identical across rounds so a provider can
/// cache the prefix.
const SYSTEM_PROMPT: &str = r#"You are AStock's financial research agent for China A-shares.

GOAL
Produce evidence-grounded research using registered tools and the deterministic Rust Engine.

OPERATING RULES
- Use registered capabilities only. Never request shell, arbitrary code, files, processes or unregistered tools.
- For current/latest/recent questions, fetch fresh market data or current disclosures before asserting a current fact, and state the data time.
- Prefer primary disclosures for company facts. Preserve conflicts, stale data, missing coverage and failed sources rather than smoothing them over.
- Never invent a market, financial, news, valuation or backtest number. When evidence is insufficient, say so.
- Use the deterministic calculation tool for material arithmetic. Do not derive a material financial number in prose.
- Use search_evidence to obtain canonical evidence identifiers. Never invent an identifier.
- Publish only through submit_report. Do not write citation markup; the runtime renders citations from the identifiers you supply.
- Write no financial figure in any prose field: statements, executive_summary, limitations, assumptions and uncertainty carry meaning, numeric_items carry figures. The runtime renders every declared number with its unit, provenance and citation, so nothing is lost to the reader.
- Choose the claim kind that is actually true: an observed fact, a deterministic calculation, an inference, an estimate, a scenario, or unknown. An estimate is not a substitute for a computation the Engine can perform.
- Seek material counter-evidence. State uncertainty and the conditions that would invalidate a conclusion.
- Graph output is seed data plus industry enrichment, not full-market coverage, and its magnitudes are documented heuristics. Distinguish "no relation collected" from "no relation exists". Never present an edge weight as measured revenue exposure.
- Backtests are research evidence only. They have not passed point-in-time, survivorship or tradability audits, so never present one as proof of profitability.
- Never place an order, route a trade, or claim a trade was executed. A trading plan is a research artifact requiring human action.

REASONING
Match effort to the task. A simple factual question needs the smallest sufficient tool path. Complex research, conflicting evidence, calculations and scenarios warrant deeper investigation. Keep private reasoning private; publish only the plan, tool actions, evidence judgements and conclusions.

FINALIZATION
When evidence is sufficient, call submit_report. Every material number must carry valid typed provenance: observed, calculated, user_assumption or estimated."#;

/// Render the prompt for one task.
///
/// The dynamic block is short and last, so the cacheable prefix stays intact.
/// `output_language` is explicit because relying on the model to infer the reply
/// language from a mostly-English control plane is exactly the kind of implicit
/// coupling that breaks quietly.
pub fn system_prompt(task: &RuntimeTask) -> String {
    format!(
        "{SYSTEM_PROMPT}\n\nTASK\ndepth={}\ntool_policy={}\noutput_language={}\nsymbol={}",
        task.depth,
        task.tool_policy,
        task.language,
        task.symbol.as_deref().unwrap_or("unspecified")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt() -> String {
        system_prompt(&RuntimeTask::ask("分析紫金矿业"))
    }

    /// The obsolete instruction to hand-format citations must be gone.
    ///
    /// It contradicted the structured contract: the model now supplies semantic
    /// identifiers and the renderer emits citation syntax. Leaving the old rule in
    /// place told the model to do a job that is no longer its own, and produced the
    /// invented namespaces seen live.
    #[test]
    fn the_prompt_does_not_ask_the_model_to_format_citations() {
        let rendered = prompt();
        assert!(
            !rendered.contains("【E:"),
            "the prompt must not instruct hand-formatted citation markup"
        );
        assert!(
            rendered.contains("Do not write citation markup"),
            "the prompt must state that the runtime renders citations"
        );
    }

    /// Publication must go through the structured contract.
    #[test]
    fn the_prompt_requires_structured_finalization() {
        let rendered = prompt();
        assert!(rendered.contains("submit_report"));
        assert!(
            rendered.contains("search_evidence"),
            "the model must be told how to obtain canonical identifiers"
        );
        assert!(
            rendered.contains("observed, calculated, user_assumption or estimated"),
            "the provenance classes must be named"
        );
    }

    /// The control plane is English; user output is Chinese.
    #[test]
    fn the_control_plane_is_english_and_user_output_is_chinese() {
        let rendered = prompt();
        assert!(
            rendered.contains("output_language=zh-CN"),
            "the reply language must be explicit, not inferred"
        );
        // The static rules carry no CJK: that is what makes the prefix stable and
        // cheap, and keeps the control plane distinct from the product's content.
        let static_part = rendered.split("\n\nTASK\n").next().unwrap_or_default();
        assert!(
            !static_part
                .chars()
                .any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "the static control-plane prompt should be English"
        );
    }

    /// Reasoning language and visibility must not be dictated.
    #[test]
    fn the_prompt_does_not_force_a_reasoning_style() {
        let rendered = prompt().to_lowercase();
        for forbidden in [
            "think step by step",
            "reason in english",
            "show your reasoning",
            "explain your thinking",
        ] {
            assert!(
                !rendered.contains(forbidden),
                "the prompt must not dictate a chain-of-thought style: `{forbidden}`"
            );
        }
        assert!(
            prompt().contains("Keep private reasoning private"),
            "private reasoning must stay private"
        );
    }

    /// Scope honesty for graph and backtests still has to be stated, because no
    /// type prevents the model from overclaiming in prose.
    #[test]
    fn the_prompt_keeps_scope_honesty_for_graph_and_backtests() {
        let rendered = prompt();
        assert!(rendered.contains("not full-market coverage"));
        assert!(rendered.contains("documented heuristics"));
        assert!(rendered.contains("no relation collected"));
        assert!(rendered.contains("measured revenue exposure"));
        assert!(rendered.contains("point-in-time"));
        assert!(rendered.contains("proof of profitability"));
    }

    /// Safety boundaries are product invariants.
    #[test]
    fn the_prompt_keeps_the_safety_boundaries() {
        let rendered = prompt();
        assert!(rendered.contains("Never request shell, arbitrary code"));
        assert!(rendered.contains("Never place an order"));
        assert!(rendered.contains("Never invent"));
    }

    /// Material arithmetic belongs in the Engine, which is the architectural fix
    /// for the 82 unreproducible prose-computed values seen live.
    #[test]
    fn the_prompt_routes_material_arithmetic_through_the_engine() {
        let rendered = prompt();
        assert!(rendered.contains("deterministic calculation tool for material arithmetic"));
        assert!(rendered.contains("Do not derive a material financial number in prose"));
        assert!(
            rendered.contains("estimate is not a substitute"),
            "estimate must not become an escape hatch"
        );
    }

    #[test]
    fn the_task_block_is_short_and_last_so_the_prefix_stays_cacheable() {
        let mut task = RuntimeTask::ask("分析紫金矿业");
        task.symbol = Some("601899".into());
        let rendered = system_prompt(&task);

        let (static_part, dynamic) = rendered
            .split_once("\n\nTASK\n")
            .expect("the dynamic block is delimited");
        assert!(dynamic.contains("symbol=601899"));
        assert!(dynamic.contains("depth=balanced"));
        // Short enough that a cache miss on it costs little.
        assert!(
            dynamic.lines().count() <= 6,
            "the dynamic block must stay compact, got {dynamic:?}"
        );
        // The static prefix must not vary with the task.
        let other = system_prompt(&RuntimeTask::ask("完全不同的问题"));
        let other_static = other.split("\n\nTASK\n").next().unwrap_or_default();
        assert_eq!(
            static_part, other_static,
            "the cacheable prefix must be byte-identical across tasks"
        );
    }

    /// The whole point of the rewrite is a smaller, stabler control plane.
    #[test]
    fn the_static_prompt_stays_compact() {
        let static_part = SYSTEM_PROMPT.chars().count();
        assert!(
            static_part < 3_000,
            "the control plane should stay a few hundred tokens, got {static_part} chars"
        );
    }
}
