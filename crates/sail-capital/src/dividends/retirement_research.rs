//! The researched early-retirement brief: the prompt sent to SAIL's local model, the strict
//! answer it must return, and the validation applied before anything is shown.
//!
//! The model is given a compact view of the deterministic [`Plan`] and of a set of
//! **fetched** source pages. It may only cite those pages, by number (`[S1]`, …); citations
//! outside the supplied set are dropped. No clock, randomness or I/O lives here: the service
//! fetches the pages, calls the model and fills the metadata.
use serde::{Deserialize, Serialize};

use super::DividendOutlook;
use super::retirement::Plan;

/// Most strategies kept from one answer.
pub const MAX_STRATEGIES: usize = 5;
/// Most risks, next steps or assumptions kept.
pub const MAX_ITEMS: usize = 8;
/// Longest excerpt from one fetched page that goes into the prompt.
pub const MAX_EXCERPT_CHARS: usize = 1_400;
/// Most sources put in front of the model.
pub const MAX_SOURCES: usize = 8;

/// A fetched page the model may cite. `text` is already extracted, not raw HTML.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceExcerpt {
    /// Page title, for the model and for error messages.
    pub title: String,
    /// Public URL the text came from.
    pub url: String,
    /// Readable text, trimmed by the service.
    pub text: String,
}

/// One source in the stored brief.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BriefSource {
    /// 1-based number; `[S<number>]` in the brief refers to it.
    pub number: u32,
    /// Page title.
    pub title: String,
    /// Public URL.
    pub url: String,
    /// RFC 3339 time the service fetched it.
    pub retrieved_at: String,
    /// Whether the brief cites it.
    pub cited: bool,
}

/// One researched strategy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Strategy {
    /// Short name.
    pub name: String,
    /// Why it fits this owner's numbers.
    pub why: String,
    /// Concrete actions.
    pub actions: Vec<String>,
    /// Costs, risks or cases where it does not fit.
    pub tradeoffs: Vec<String>,
    /// Source numbers cited, each within the supplied set.
    pub citations: Vec<u32>,
}

/// What the model returned after validation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Draft {
    /// One-line headline.
    pub headline: String,
    /// Two or three plain sentences.
    pub summary: String,
    /// Researched strategies.
    pub strategies: Vec<Strategy>,
    /// Risks and limits.
    pub risks: Vec<String>,
    /// Concrete next steps.
    pub next_steps: Vec<String>,
    /// Assumptions the plan depends on.
    pub assumptions: Vec<String>,
}

/// The stored brief, with everything the page needs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Brief {
    /// One-line headline.
    pub headline: String,
    /// Two or three plain sentences.
    pub summary: String,
    /// Researched strategies.
    pub strategies: Vec<Strategy>,
    /// Risks and limits.
    pub risks: Vec<String>,
    /// Concrete next steps.
    pub next_steps: Vec<String>,
    /// Assumptions the plan depends on.
    pub assumptions: Vec<String>,
    /// Sources offered to the model, with the cited flag.
    pub sources: Vec<BriefSource>,
    /// Model that wrote it.
    pub model: String,
    /// RFC 3339 time it finished.
    pub generated_at: String,
    /// Whether the answer was valid JSON inventory; when false, `summary` is the model's
    /// prose and the other lists are empty.
    pub structured: bool,
    /// Method and limits, written by SAIL, not the model.
    pub notes: Vec<String>,
}

/// The system message. It fixes the role, the citations rule and the JSON shape.
pub fn system_prompt() -> String {
    format!(
        "You are SAIL's local retirement research assistant. You write one short planning \
brief for the owner of a dividend portfolio who wants to stop working early.\n\
Rules:\n\
- Use only the numbered sources supplied in the user message for factual claims, and cite \
them as [S1], [S2] and so on. Never cite a number that is not in the list, and never invent \
a source, a URL or a study.\n\
- The deterministic plan numbers are the owner's; do not recompute or contradict them. You \
may explain what they imply.\n\
- Be concrete and conservative. Say when a strategy would not fit these numbers. No hype, \
no certainty about future returns, no product promotion.\n\
- This is planning information, not regulated financial advice.\n\
- Keep it short: every string under 35 words, at most 3 actions and 3 tradeoffs per \
strategy, so the whole answer fits.\n\
- Reply with JSON only, no markdown fences, using exactly these keys: headline (string, one \
line), summary (string, 2-3 sentences), strategies (array of at most {MAX_STRATEGIES} \
objects with name, why, actions (array of strings), tradeoffs (array of strings), citations \
(array of integers)), risks (array of strings, at most {MAX_ITEMS}), next_steps (array of \
strings, at most {MAX_ITEMS}), assumptions (array of strings, at most {MAX_ITEMS})."
    )
}

/// The user message: the plan, the projection's shape, and the fetched sources.
pub fn user_prompt(plan: &Plan, outlook: &DividendOutlook, sources: &[SourceExcerpt]) -> String {
    let mut out = String::with_capacity(12_000);
    out.push_str("THE OWNER'S DETERMINISTIC PLAN\n");
    out.push_str(&format!(
        "- Age today {}; wants income to cover spending at age {} ({} years, projection year {}).\n",
        plan.assumptions.current_age,
        plan.assumptions.retire_age,
        plan.years_to_retirement,
        plan.retirement_year
    ));
    out.push_str(&format!(
        "- Target now: {:.0} a month net; at retirement: {:.0} a month nominal ({}% inflation).\n",
        plan.assumptions.target_monthly_net,
        plan.target_monthly_at_retirement,
        plan.assumptions.inflation_pct
    ));
    out.push_str(&format!(
        "- Capital target at a {}% withdrawal rate: {:.0}.\n",
        plan.assumptions.withdrawal_rate_pct, plan.required_capital
    ));
    out.push_str(&format!(
        "- Income today: {:.0} a month net, {:.1}% of the target. Projection allowance: {:.0}/month.\n",
        plan.base_monthly_net_today, plan.progress_pct, plan.current_monthly_contribution
    ));
    match plan.required_monthly_contribution {
        Some(needed) => out.push_str(&format!(
            "- Allowance needed to fund the income target in the base scenario: {needed:.0}/month.\n"
        )),
        None => out.push_str(
            "- Even the maximum allowance does not fund the income target by that age in the base scenario.\n",
        ),
    }
    for scenario in &plan.scenarios {
        out.push_str(&format!(
            "- {scenario:?}: monthly net at retirement {:.0} in today's money; portfolio {:.0}; \
income target met by age {}; funded by income: {}; funded by capital: {}.\n",
            scenario.monthly_net_today,
            scenario.value_at_retirement,
            scenario
                .income_reached_age
                .map(|age| age.to_string())
                .unwrap_or_else(|| "not within the window".to_string()),
            if scenario.funded_by_income {
                "yes"
            } else {
                "no"
            },
            if scenario.funded_by_capital {
                "yes"
            } else {
                "no"
            },
        ));
    }
    out.push_str("\nPROJECTION TRAJECTORY (base scenario, net income per year)\n");
    if let Some(base) = outlook
        .scenarios
        .iter()
        .find(|projection| projection.scenario == super::Scenario::Base)
    {
        for year in [1_u32, 5, 10, 20, 30] {
            if let Some(row) = base.years.get((year - 1) as usize) {
                out.push_str(&format!(
                    "- Year {year}: net {:.0}; reinvested {:.0}; contributed {:.0}; paying value {:.0}.\n",
                    row.net, row.reinvested, row.contributed, row.value
                ));
            }
        }
    }
    out.push_str("\nHOLDINGS BY PROJECTED INCOME (base scenario)\n");
    let mut holdings: Vec<&super::HoldingRow> = outlook.holdings.iter().collect();
    holdings.sort_by(|a, b| {
        b.first_year_net
            .partial_cmp(&a.first_year_net)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if holdings.is_empty() {
        out.push_str("- None with a payout history.\n");
    }
    for holding in holdings.iter().take(6) {
        out.push_str(&format!(
            "- {}: year-1 net {:.0}; expected payout growth {:.1}%/year; making up the mix matters more than adding a single name.\n",
            holding.symbol,
            holding.first_year_net,
            holding.growth.g * 100.0
        ));
    }
    out.push_str("\nFETCHED SOURCES (cite only these)\n");
    if sources.is_empty() {
        out.push_str("- None could be fetched; say so and give no factual citations.\n");
    }
    for (index, source) in sources.iter().take(MAX_SOURCES).enumerate() {
        let text = truncate(&source.text, MAX_EXCERPT_CHARS);
        out.push_str(&format!(
            "\n[S{}] {} — {}\n{}\n",
            index + 1,
            source.title,
            source.url,
            text
        ));
    }
    out.push_str(
        "\nWrite the brief for these exact numbers and sources. Explain how to raise the \
income or capital in the projection (reinvestment, allocation, allowance, holding mix, \
taxes), what would make the date later, and what to verify before acting. Include at least \
one concrete check the owner can run in SAIL.",
    );
    out
}

/// What the page shows when the model's answer was JSON that could not be read at all:
/// never the raw JSON itself.
pub const UNREADABLE_ANSWER: &str = "The local model's answer was cut off before it could be \
read. Press \"Research a retirement strategy\" to run it again.";

/// A validated draft and whether it came back as structured JSON. An answer cut off
/// mid-JSON keeps its complete parts ([`parse_structured`] repairs it). An answer that is
/// JSON but unreadable becomes [`UNREADABLE_ANSWER`]; plain prose is kept as the summary.
pub fn draft_from_response(raw: &str, source_count: usize) -> (Draft, bool) {
    match parse_structured(raw, source_count) {
        Some(draft) => (draft, true),
        None => {
            let trimmed = raw.trim();
            let summary = if looks_like_json(trimmed) {
                UNREADABLE_ANSWER.to_string()
            } else {
                truncate(trimmed, 4_000)
            };
            (
                Draft {
                    headline: "Research note".into(),
                    summary,
                    ..Draft::default()
                },
                false,
            )
        }
    }
}

/// Whether the model's answer was cut off: its JSON does not parse as it is, but does
/// once the unfinished tail is dropped and the brackets are closed.
pub fn was_cut_off(raw: &str) -> bool {
    let Some(start) = raw.find('{') else {
        return false;
    };
    let strict =
        json_object(raw).is_some_and(|json| serde_json::from_str::<RawDraft>(json).is_ok());
    !strict && repaired_draft(&raw[start..]).is_some()
}

/// Text that starts like the JSON answer (an object, possibly fenced).
fn looks_like_json(text: &str) -> bool {
    let body = text
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_start();
    body.starts_with('{') || body.contains("\"headline\"")
}

/// The brief as plain sentences for text to speech: headline, summary, each strategy
/// with its reason and actions, then next steps and risks. Source markers like `[S2]`
/// are dropped, since they mean nothing when heard.
pub fn speakable(draft: &Draft) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut push = |text: &str| {
        let clean = strip_source_markers(text);
        let clean = clean.trim();
        if !clean.is_empty() {
            let ends = clean.ends_with(['.', '!', '?']);
            parts.push(if ends {
                clean.to_string()
            } else {
                format!("{clean}.")
            });
        }
    };
    push(&draft.headline);
    push(&draft.summary);
    for (index, strategy) in draft.strategies.iter().enumerate() {
        push(&format!("Strategy {}: {}", index + 1, strategy.name));
        push(&strategy.why);
        for action in &strategy.actions {
            push(action);
        }
    }
    if !draft.next_steps.is_empty() {
        push("Next steps");
        for step in &draft.next_steps {
            push(step);
        }
    }
    if !draft.risks.is_empty() {
        push("Risks");
        for risk in &draft.risks {
            push(risk);
        }
    }
    parts.join(" ")
}

/// Drops `[S1]`, `[S2, S3]` and similar source markers.
fn strip_source_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("[S") {
        let (before, after) = rest.split_at(open);
        match after.find(']') {
            Some(close)
                if after[1..close]
                    .chars()
                    .all(|c| c == 'S' || c == ',' || c == ' ' || c.is_ascii_digit()) =>
            {
                out.push_str(before.trim_end());
                rest = &after[close + 1..];
            }
            _ => {
                out.push_str(before);
                out.push('[');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Deserialize, Default)]
struct RawDraft {
    #[serde(default)]
    headline: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    strategies: Vec<RawStrategy>,
    #[serde(default)]
    risks: Vec<String>,
    #[serde(default)]
    next_steps: Vec<String>,
    #[serde(default)]
    assumptions: Vec<String>,
}

#[derive(Deserialize, Default)]
struct RawStrategy {
    #[serde(default)]
    name: String,
    #[serde(default)]
    why: String,
    #[serde(default)]
    actions: Vec<String>,
    #[serde(default)]
    tradeoffs: Vec<String>,
    #[serde(default)]
    citations: Vec<i64>,
}

/// Parse the model's JSON (tolerating code fences and prose around it). `None` when no
/// usable object is found.
pub fn parse_structured(raw: &str, source_count: usize) -> Option<Draft> {
    let strict = json_object(raw).and_then(|json| serde_json::from_str::<RawDraft>(json).ok());
    let parsed = match strict {
        Some(parsed) => parsed,
        None => repaired_draft(&raw[raw.find('{')?..])?,
    };
    let headline = one_line(&parsed.headline, 160);
    let summary = truncate(parsed.summary.trim(), 1_500);
    if headline.is_empty() && summary.is_empty() {
        return None;
    }
    let strategies: Vec<Strategy> = parsed
        .strategies
        .into_iter()
        .filter_map(|strategy| {
            let name = one_line(&strategy.name, 120);
            let why = truncate(strategy.why.trim(), 700);
            if name.is_empty() || why.is_empty() {
                return None;
            }
            let mut citations: Vec<u32> = strategy
                .citations
                .into_iter()
                .filter(|number| *number >= 1 && (*number as usize) <= source_count)
                .map(|number| number as u32)
                .collect();
            citations.sort_unstable();
            citations.dedup();
            Some(Strategy {
                name,
                why,
                actions: list(strategy.actions, 300),
                tradeoffs: list(strategy.tradeoffs, 300),
                citations,
            })
        })
        .take(MAX_STRATEGIES)
        .collect();
    if strategies.is_empty() && headline.is_empty() {
        return None;
    }
    Some(Draft {
        headline,
        summary,
        strategies,
        risks: list(parsed.risks, 320),
        next_steps: list(parsed.next_steps, 320),
        assumptions: list(parsed.assumptions, 320),
    })
}

/// The JSON object inside a model answer: fenced JSON, bare JSON, or JSON with prose around.
fn json_object(raw: &str) -> Option<&str> {
    let raw = raw.trim();
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    (end > start).then(|| &raw[start..=end])
}

/// Reads an answer cut off mid-JSON: tries every point where a value had just closed,
/// latest first, drops the unfinished tail after it and closes the open brackets. The
/// first prefix that parses wins, so only complete strings, strategies and lists are
/// kept.
fn repaired_draft(text: &str) -> Option<RawDraft> {
    // (byte index just after a closing bracket or string, the brackets open there)
    let mut cuts: Vec<(usize, Vec<u8>)> = Vec::new();
    let mut stack: Vec<u8> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in text.bytes().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
                cuts.push((index + 1, stack.clone()));
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => stack.push(byte),
            b'}' | b']' => {
                stack.pop();
                cuts.push((index + 1, stack.clone()));
                if stack.is_empty() {
                    break;
                }
            }
            _ => {}
        }
    }
    // Bound the work on a long answer: the last 400 cut points are plenty.
    let skip = cuts.len().saturating_sub(400);
    for (end, open) in cuts.into_iter().skip(skip).rev() {
        let mut candidate = text[..end].trim_end().trim_end_matches(',').to_string();
        for bracket in open.iter().rev() {
            candidate.push(if *bracket == b'{' { '}' } else { ']' });
        }
        if let Ok(parsed) = serde_json::from_str::<RawDraft>(&candidate) {
            return Some(parsed);
        }
    }
    None
}

/// First line only, collapsed, at most `max` characters.
fn one_line(text: &str, max: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&collapsed, max)
}

/// Clean, de-duplicate, cap count and length.
fn list(items: Vec<String>, max_len: usize) -> Vec<String> {
    let mut out = Vec::new();
    for item in items {
        let cleaned = one_line(&item, max_len);
        if !cleaned.is_empty() && !out.contains(&cleaned) {
            out.push(cleaned);
            if out.len() >= MAX_ITEMS {
                break;
            }
        }
    }
    out
}

/// Truncate on a character boundary without an ellipsis (the UI adds its own).
pub fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sources() -> Vec<SourceExcerpt> {
        vec![
            SourceExcerpt {
                title: "Safe withdrawal rates".into(),
                url: "https://example.org/swr".into(),
                text: "A 4% rule study. ".repeat(300),
            },
            SourceExcerpt {
                title: "Portuguese tax".into(),
                url: "https://example.org/pt".into(),
                text: "Dividends are taxed at 28%.".into(),
            },
        ]
    }

    fn plan() -> Plan {
        serde_json::from_value(serde_json::json!({
            "assumptions": {
                "current_age": 40, "retire_age": 55, "target_monthly_net": 2500.0,
                "inflation_pct": 2.0, "withdrawal_rate_pct": 4.0
            },
            "years_to_retirement": 15,
            "retirement_year": 15,
            "target_monthly_at_retirement": 3364.0,
            "target_annual_at_retirement": 40368.0,
            "required_capital": 1009200.0,
            "progress_pct": 31.2,
            "base_monthly_net_today": 780.0,
            "current_monthly_contribution": 300.0,
            "required_monthly_contribution": 850.0,
            "scenarios": [],
            "notes": []
        }))
        .expect("plan fixture")
    }

    fn outlook() -> DividendOutlook {
        serde_json::from_value(serde_json::json!({
            "settings": {
                "years": 30, "reinvest_pct": 100.0, "allocation": "proportional",
                "top_n": 5, "monthly_contribution": 300.0, "reserve_pt_tax": false,
                "start": "2026-01-01"
            },
            "scenarios": [{
                "scenario": "base",
                "years": [
                    {"year": 1, "gross": 9000.0, "withheld": 900.0, "pt_tax": 0.0, "net": 8100.0,
                     "reinvested": 0.0, "contributed": 3600.0, "cash": 0.0, "value": 100000.0,
                     "yield_on_cost": null},
                    {"year": 5, "gross": 12000.0, "withheld": 1200.0, "pt_tax": 0.0, "net": 10800.0,
                     "reinvested": 0.0, "contributed": 3600.0, "cash": 0.0, "value": 140000.0,
                     "yield_on_cost": null},
                    {"year": 10, "gross": 16000.0, "withheld": 1600.0, "pt_tax": 0.0, "net": 14400.0,
                     "reinvested": 0.0, "contributed": 3600.0, "cash": 0.0, "value": 190000.0,
                     "yield_on_cost": null},
                    {"year": 15, "gross": 21000.0, "withheld": 2100.0, "pt_tax": 0.0, "net": 18900.0,
                     "reinvested": 0.0, "contributed": 3600.0, "cash": 0.0, "value": 250000.0,
                     "yield_on_cost": null},
                    {"year": 20, "gross": 27000.0, "withheld": 2700.0, "pt_tax": 0.0, "net": 24300.0,
                     "reinvested": 0.0, "contributed": 3600.0, "cash": 0.0, "value": 320000.0,
                     "yield_on_cost": null}
                ],
                "monthly_net": [], "doubling_year": null
            }],
            "holdings": [{
                "symbol": "PAY", "first_year_net": 8100.0, "last_year_net": 20000.0,
                "end_units": 1000.0,
                "growth": {"annual": [], "raw": 0.05, "g": 0.04, "sigma": 0.1, "months": [1,2,3,4,5,6,7,8,9,10,11,12], "ttm_per_share": 1.2},
                "withholding": [0.15, "observed"]
            }],
            "non_paying": [],
            "without_reinvestment": {
                "scenario": "base", "years": [], "monthly_net": [], "doubling_year": null
            },
            "notes": []
        }))
        .expect("outlook fixture")
    }

    #[test]
    fn the_prompt_is_deterministic_and_cites_every_source() {
        let prompt = user_prompt(&plan(), &outlook(), &sources());
        assert_eq!(prompt, user_prompt(&plan(), &outlook(), &sources()));
        assert!(prompt.contains("[S1]"));
        assert!(prompt.contains("[S2]"));
        assert!(prompt.contains("850"));
        assert!(prompt.contains("Portuguese tax"));
        assert!(
            prompt.len() < 20_000,
            "prompt stays compact: {}",
            prompt.len()
        );
        let system = system_prompt();
        assert!(system.contains("Never cite"));
        assert!(system.contains("[S1]"));
    }

    #[test]
    fn parses_fenced_json_and_drops_unknown_citations() {
        let raw = format!(
            "Here is the brief:\n```json\n{}\n```",
            serde_json::json!({
                "headline": "Feed the income engine",
                "summary": "Your gap is real.",
                "strategies": [
                    {
                        "name": "Raise the allowance",
                        "why": "The projection needs it.",
                        "actions": ["Add 200 a month", "Add 200 a month"],
                        "tradeoffs": ["Less cash today"],
                        "citations": [1, 9, 2, 2]
                    },
                    {"name": "", "why": "ignored"},
                    {
                        "name": "Keep the mix",
                        "why": "Concentration grows.",
                        "actions": [],
                        "tradeoffs": [],
                        "citations": []
                    }
                ],
                "risks": ["A cut delays the date", "A cut delays the date"],
                "next_steps": ["Re-run the projection"],
                "assumptions": ["2% inflation"],
                "extra": "ignored"
            })
        );
        let draft = parse_structured(&raw, 2).expect("structured");
        assert_eq!(draft.headline, "Feed the income engine");
        assert_eq!(draft.strategies.len(), 2);
        assert_eq!(draft.strategies[0].citations, vec![1, 2]);
        assert_eq!(draft.strategies[0].actions, vec!["Add 200 a month"]);
        assert_eq!(draft.risks.len(), 1);
        assert_eq!(draft.assumptions, vec!["2% inflation"]);
    }

    #[test]
    fn an_answer_cut_off_mid_json_keeps_its_complete_parts() {
        let full = r#"{"headline": "Close the gap", "summary": "You need more savings.",
            "strategies": [
              {"name": "Raise the allowance", "why": "It is the biggest lever [S3].",
               "actions": ["Add 200 a month"], "tradeoffs": ["Less to spend"], "citations": [3]},
              {"name": "Reinvest", "why": "Compounding", "actions": ["Turn on DRIP"],
               "tradeoffs": [], "citations": [4]},
              {"name": "Optimize Tax Efficiency", "why": "Holding international ETFs (XMPL.DE"#;
        assert!(parse_structured(full, 4).is_some());
        assert!(was_cut_off(full));
        let (draft, structured) = draft_from_response(full, 4);
        assert!(structured);
        assert_eq!(draft.headline, "Close the gap");
        // The unfinished third strategy is dropped; the two complete ones stay.
        assert_eq!(draft.strategies.len(), 2);
        assert_eq!(draft.strategies[1].name, "Reinvest");
        assert!(!was_cut_off(r#"{"headline": "a", "summary": "b"}"#));
        assert!(!was_cut_off("plain prose"));
    }

    #[test]
    fn unreadable_json_is_never_shown_raw() {
        let (draft, structured) = draft_from_response(r#"{"headline": "#, 2);
        assert!(!structured);
        assert_eq!(draft.summary, UNREADABLE_ANSWER);
        let (fenced, _) = draft_from_response("```json\n{\"oops\"", 2);
        assert_eq!(fenced.summary, UNREADABLE_ANSWER);
    }

    #[test]
    fn the_spoken_brief_is_plain_sentences_without_source_markers() {
        let draft = Draft {
            headline: "Close the gap".into(),
            summary: "You need more savings [S1].".into(),
            strategies: vec![Strategy {
                name: "Raise the allowance".into(),
                why: "It is the biggest lever [S2, S3]".into(),
                actions: vec!["Add 200 a month".into()],
                tradeoffs: vec!["Less to spend".into()],
                citations: vec![2, 3],
            }],
            risks: vec!["Returns may be lower".into()],
            next_steps: vec!["Check the plan in a year".into()],
            assumptions: Vec::new(),
        };
        let spoken = speakable(&draft);
        assert_eq!(
            spoken,
            "Close the gap. You need more savings. Strategy 1: Raise the allowance. It is the \
             biggest lever. Add 200 a month. Next steps. Check the plan in a year. Risks. \
             Returns may be lower."
        );
        assert!(!spoken.contains('{') && !spoken.contains("[S"));
        assert_eq!(strip_source_markers("keep [this] text"), "keep [this] text");
    }

    #[test]
    fn non_json_answers_become_an_unstructured_note() {
        let (draft, structured) = draft_from_response("Buy dividends, retire early.", 2);
        assert!(!structured);
        assert_eq!(draft.headline, "Research note");
        assert_eq!(draft.summary, "Buy dividends, retire early.");
        assert!(draft.strategies.is_empty());
    }

    #[test]
    fn hostile_sizes_are_capped() {
        let raw = serde_json::json!({
            "headline": "x".repeat(1_000),
            "summary": "y".repeat(10_000),
            "strategies": (0..40).map(|index| serde_json::json!({
                "name": format!("s{index}"),
                "why": "w".repeat(9_000),
                "actions": vec!["a".repeat(9_000); 40],
                "tradeoffs": [],
                "citations": [1, 1, 1]
            })).collect::<Vec<_>>(),
            "risks": vec!["r".repeat(9_000); 40],
            "next_steps": [],
            "assumptions": []
        });
        let draft = parse_structured(&raw.to_string(), 1).expect("structured");
        assert_eq!(draft.headline.chars().count(), 160);
        assert_eq!(draft.strategies.len(), MAX_STRATEGIES);
        assert!(draft.strategies[0].why.chars().count() <= 700);
        assert!(draft.strategies[0].actions.len() <= MAX_ITEMS);
        assert_eq!(draft.risks.len(), 1, "identical items are deduplicated");
        assert!(draft.summary.chars().count() <= 1_500);
    }

    #[test]
    fn truncation_is_character_safe() {
        let text = "€€€";
        assert_eq!(truncate(text, 2), "€€");
        assert_eq!(truncate("abc", 10), "abc");
    }
}
