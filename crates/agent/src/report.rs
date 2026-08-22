//! Final-answer post-processing: conclusion grading and the evidence (tool
//! provenance) list. No disclaimer is appended — the UI shows a permanent
//! one, and the system prompt forbids boilerplate to save tokens.

use serde::{Deserialize, Serialize};

/// Provenance of one tool execution that fed the final answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    /// Tool that produced the data.
    pub tool: String,
    /// Cache key under which the full payload is stored.
    pub cache_key: String,
    /// Upstream data source.
    pub source: String,
    /// Fetch time of the underlying data, RFC 3339.
    pub fetched_at: String,
}

/// One graded conclusion line parsed from the model's answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conclusion {
    /// Grade label: 事实 / 计算 / 外部 / 推断 / 假设.
    pub grade: String,
    /// The conclusion text after the grade marker.
    pub text: String,
}

/// The assembled final report of an agent task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentReport {
    /// Owning task id.
    pub task_id: String,
    /// Final answer text (no boilerplate appended — UI carries a fixed
    /// disclaimer).
    pub answer: String,
    /// Graded conclusion blocks, when the model emitted them.
    pub conclusions: Vec<Conclusion>,
    /// Tool provenance backing the answer.
    pub evidence: Vec<Evidence>,
    /// Assembly time, unix seconds.
    pub generated_at: i64,
}

/// Recognized conclusion grade markers.
const GRADES: &[&str] = &["事实", "计算", "外部", "推断", "假设"];

/// Parse `【级别】...` lines out of the model's answer.
pub fn parse_conclusions(text: &str) -> Vec<Conclusion> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        for grade in GRADES {
            let marker = format!("【{grade}】");
            if let Some(rest) = line.strip_prefix(marker.as_str()) {
                let text = rest.trim();
                if !text.is_empty() {
                    out.push(Conclusion {
                        grade: (*grade).to_string(),
                        text: text.to_string(),
                    });
                }
                break;
            }
        }
    }
    out
}

/// Assemble the final report: graded conclusions extracted, evidence
/// attached. The answer text is used as-is.
pub fn assemble_report(
    task_id: &str,
    final_text: &str,
    evidence: Vec<Evidence>,
    generated_at: i64,
) -> AgentReport {
    AgentReport {
        task_id: task_id.to_string(),
        answer: final_text.trim_end().to_string(),
        conclusions: parse_conclusions(final_text),
        evidence,
        generated_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_graded_conclusions() {
        let text = "分析如下\n【事实】收盘价 1800.5 元\n【计算】综合评分 72 分\n【推断】短线偏强\n备注行不解析";
        let c = parse_conclusions(text);
        assert_eq!(c.len(), 3);
        assert_eq!(c[0].grade, "事实");
        assert_eq!(c[1].grade, "计算");
        assert_eq!(c[2].grade, "推断");
        assert_eq!(c[2].text, "短线偏强");
    }

    #[test]
    fn answer_is_used_without_boilerplate() {
        let report = assemble_report("t1", "结论。【事实】x\n", vec![], 0);
        assert_eq!(report.answer, "结论。【事实】x");
        assert!(!report.answer.contains("免责声明"));
    }

    #[test]
    fn report_roundtrip() {
        let evidence = vec![Evidence {
            tool: "get_quote".into(),
            cache_key: "get_quote:abc".into(),
            source: "eastmoney".into(),
            fetched_at: "2026-08-21T03:00:00Z".into(),
        }];
        let report = assemble_report("t1", "【计算】评分 72", evidence.clone(), 123);
        assert_eq!(report.task_id, "t1");
        assert_eq!(report.conclusions.len(), 1);
        assert_eq!(report.evidence, evidence);
        assert!(!report.answer.contains("免责声明"));
        let json = serde_json::to_string(&report).unwrap();
        let back: AgentReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, report);
    }
}
