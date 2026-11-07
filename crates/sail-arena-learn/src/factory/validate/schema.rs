//! Schema validator (`CheckKind::Schema`): structural integrity, length bounds, fixture
//! linkage, and executable-markup rejection.
//!
//! Even though clients render lesson blocks using `textContent`, the server rejects drafts
//! containing `<script`, `javascript:`, `on*=` event handlers, `<iframe`, raw HTML tags in
//! prose blocks, or `data:` URLs as defence in depth.

use super::make_receipt;
use crate::courses::content::{
    AnswerKey, Block, BlockKind, Chapter, ChoiceOption, CoursePackage, Domain, MAX_HINTS,
    Objective, Step, StepKind,
};
use crate::factory::contracts::{
    CheckKind, DraftValidator, LessonDraft, ValidationContext, ValidationReceipt, Verdict,
};
use std::collections::BTreeSet;

/// Version tag stamped on receipts produced by [`SchemaValidator`].
pub const VALIDATOR_VERSION: &str = "schema/1";

/// Maximum number of steps permitted in a single lesson.
pub const MAX_STEPS_PER_LESSON: usize = 32;

/// Maximum number of review variants permitted in a single draft.
pub const MAX_REVIEW_VARIANTS_PER_DRAFT: usize = 32;

/// Maximum number of objectives a single lesson may target.
pub const MAX_OBJECTIVES_PER_LESSON: usize = 16;

/// Maximum number of blocks (`p`, `h`, `code`, `note`) per step field.
pub const MAX_BLOCKS_PER_STEP: usize = 32;

/// Maximum number of options or orderable items in a step.
pub const MAX_OPTIONS_PER_STEP: usize = 16;

/// Maximum number of factual claims attached to a lesson draft.
pub const MAX_CLAIMS_PER_DRAFT: usize = 128;

/// Maximum number of subject fixtures per category in a lesson draft.
pub const MAX_FIXTURES_PER_DRAFT: usize = 32;

/// Maximum byte length of an identifier (`id`, `step_id`, `objective_id`, `family`).
pub const MAX_ID_BYTES: usize = 128;

/// Maximum byte length of a lesson or step title.
pub const MAX_TITLE_BYTES: usize = 240;

/// Maximum byte length of a single prose or code block.
pub const MAX_TEXT_BLOCK_BYTES: usize = 8 * 1024;

/// Maximum byte length of a choice option, order item, hint, or misconception string.
pub const MAX_SHORT_TEXT_BYTES: usize = 1024;

/// Maximum byte length of code templates or starters.
pub const MAX_CODE_FIELD_BYTES: usize = crate::courses::grade::MAX_CODE;

/// Validates draft structure, `CoursePackage::validate` invariants, fixture linkage, size
/// caps, and absence of executable markup.
#[derive(Debug, Default, Clone, Copy)]
pub struct SchemaValidator;

#[async_trait::async_trait]
impl DraftValidator for SchemaValidator {
    fn check(&self) -> CheckKind {
        CheckKind::Schema
    }

    async fn validate(
        &self,
        draft: &LessonDraft,
        ctx: &ValidationContext<'_>,
    ) -> Vec<ValidationReceipt> {
        let reasons = validate_schema(draft);
        let verdict = if reasons.is_empty() {
            Verdict::Pass
        } else {
            Verdict::Fail { reasons }
        };
        vec![make_receipt(
            draft,
            CheckKind::Schema,
            None,
            verdict,
            VALIDATOR_VERSION,
            ctx.now_ms,
        )]
    }
}

/// Runs all deterministic schema checks on `draft`, returning every failure reason found.
pub fn validate_schema(draft: &LessonDraft) -> Vec<String> {
    let mut errors = Vec::new();
    let lesson = &draft.output.lesson;

    // 1. Top-level lesson identifiers and counts.
    check_id(&lesson.id, "lesson.id", &mut errors);
    check_non_empty_bounded(
        &lesson.title,
        "lesson.title",
        MAX_TITLE_BYTES,
        true,
        &mut errors,
    );

    if lesson.objective_ids.is_empty() {
        errors.push("lesson must declare at least one objective_id".to_string());
    } else if lesson.objective_ids.len() > MAX_OBJECTIVES_PER_LESSON {
        errors.push(format!(
            "lesson has {} objectives, exceeding cap of {MAX_OBJECTIVES_PER_LESSON}",
            lesson.objective_ids.len()
        ));
    }

    let mut obj_set = BTreeSet::new();
    for obj_id in &lesson.objective_ids {
        check_id(obj_id, "lesson.objective_ids", &mut errors);
        if !obj_set.insert(obj_id.as_str()) {
            errors.push(format!("duplicate lesson objective_id `{obj_id}`"));
        }
    }

    let mut prereq_set = BTreeSet::new();
    for prereq in &lesson.prerequisites {
        check_id(prereq, "lesson.prerequisites", &mut errors);
        if prereq == &lesson.id {
            errors.push(format!(
                "lesson `{}` cannot list itself as a prerequisite",
                lesson.id
            ));
        }
        if !prereq_set.insert(prereq.as_str()) {
            errors.push(format!("duplicate lesson prerequisite `{prereq}`"));
        }
    }

    if lesson.steps.is_empty() {
        errors.push("lesson must contain at least one step".to_string());
    } else if lesson.steps.len() > MAX_STEPS_PER_LESSON {
        errors.push(format!(
            "lesson has {} steps, exceeding cap of {MAX_STEPS_PER_LESSON}",
            lesson.steps.len()
        ));
    }

    if draft.output.review_variants.len() > MAX_REVIEW_VARIANTS_PER_DRAFT {
        errors.push(format!(
            "draft has {} review_variants, exceeding cap of {MAX_REVIEW_VARIANTS_PER_DRAFT}",
            draft.output.review_variants.len()
        ));
    }

    if draft.output.claims.len() > MAX_CLAIMS_PER_DRAFT {
        errors.push(format!(
            "draft has {} claims, exceeding cap of {MAX_CLAIMS_PER_DRAFT}",
            draft.output.claims.len()
        ));
    }

    // 2. Step-level bounds, unique IDs across lesson + review variants, and markup scanning.
    let mut all_step_ids = BTreeSet::new();
    for step in lesson
        .steps
        .iter()
        .chain(draft.output.review_variants.iter())
    {
        if !all_step_ids.insert(step.id.as_str()) {
            errors.push(format!("duplicate step id `{}` in draft", step.id));
        }
        validate_step_details(step, &mut errors);
    }

    // 3. Wrap the draft lesson in a one-lesson CoursePackage and call CoursePackage::validate.
    let objectives: Vec<Objective> = lesson
        .objective_ids
        .iter()
        .map(|id| Objective {
            id: id.clone(),
            title: id.clone(),
            prerequisites: Vec::new(),
        })
        .collect();

    let mut reviews = draft.output.review_variants.clone();
    for (idx, obj_id) in lesson.objective_ids.iter().enumerate() {
        let covered = reviews.iter().any(|r| r.objective_ids.contains(obj_id));
        if !covered {
            reviews.push(synthetic_review_for_objective(obj_id, idx));
        }
    }

    let mut wrapped_lesson = lesson.clone();
    wrapped_lesson.prerequisites.clear();

    let package = CoursePackage {
        id: if draft.course_id.is_empty() {
            "draft-course".to_string()
        } else {
            draft.course_id.clone()
        },
        version: "0.1.0-draft".to_string(),
        title: lesson.title.clone(),
        domain: Domain::Rust,
        summary: lesson.title.clone(),
        objectives,
        chapters: vec![Chapter {
            id: "draft-chapter".to_string(),
            title: lesson.title.clone(),
            lessons: vec![lesson.id.clone()],
        }],
        lessons: vec![wrapped_lesson],
        diagnostic: None,
        reviews,
    };

    if let Err(pkg_err) = package.validate() {
        errors.push(format!("CoursePackage::validate failed: {pkg_err}"));
    }

    // 4. Validate fixture linkage and bounds.
    validate_fixtures(draft, &mut errors);

    // 5. Scan claims, follow_up, and limitations for executable markup and bounds.
    for (idx, claim) in draft.output.claims.iter().enumerate() {
        check_non_empty_bounded(
            &claim.text,
            &format!("claims[{idx}].text"),
            MAX_TEXT_BLOCK_BYTES,
            true,
            &mut errors,
        );
        if let Some(deriv) = &claim.derivation {
            check_bounded(
                deriv,
                &format!("claims[{idx}].derivation"),
                MAX_TEXT_BLOCK_BYTES,
                false,
                &mut errors,
            );
        }
    }
    if let Some(follow_up) = &draft.output.follow_up {
        check_bounded(
            follow_up,
            "follow_up",
            MAX_TEXT_BLOCK_BYTES,
            true,
            &mut errors,
        );
    }
    for (idx, lim) in draft.output.limitations.iter().enumerate() {
        check_bounded(
            lim,
            &format!("limitations[{idx}]"),
            MAX_SHORT_TEXT_BYTES,
            true,
            &mut errors,
        );
    }

    errors
}

fn validate_step_details(step: &Step, errors: &mut Vec<String>) {
    let ctx = format!("step `{}`", step.id);
    check_id(&step.id, &format!("{ctx}.id"), errors);
    check_non_empty_bounded(
        &step.title,
        &format!("{ctx}.title"),
        MAX_TITLE_BYTES,
        true,
        errors,
    );

    if let Some(family) = &step.family {
        check_id(family, &format!("{ctx}.family"), errors);
    }

    if step.hints.len() > MAX_HINTS {
        errors.push(format!(
            "{ctx}: has {} hints, exceeding MAX_HINTS ({MAX_HINTS})",
            step.hints.len()
        ));
    }
    for (i, hint) in step.hints.iter().enumerate() {
        check_non_empty_bounded(
            hint,
            &format!("{ctx}.hints[{i}]"),
            MAX_SHORT_TEXT_BYTES,
            true,
            errors,
        );
    }

    validate_blocks(&step.body, &format!("{ctx}.body"), errors);
    validate_blocks(&step.correct, &format!("{ctx}.correct"), errors);
    validate_blocks(&step.solution, &format!("{ctx}.solution"), errors);

    if step.options.len() > MAX_OPTIONS_PER_STEP {
        errors.push(format!(
            "{ctx}: has {} options, exceeding cap of {MAX_OPTIONS_PER_STEP}",
            step.options.len()
        ));
    }
    let mut opt_ids = BTreeSet::new();
    for opt in &step.options {
        validate_choice_option(opt, &format!("{ctx}.options"), &mut opt_ids, errors);
    }

    if step.items.len() > MAX_OPTIONS_PER_STEP {
        errors.push(format!(
            "{ctx}: has {} items, exceeding cap of {MAX_OPTIONS_PER_STEP}",
            step.items.len()
        ));
    }
    let mut item_ids = BTreeSet::new();
    for item in &step.items {
        validate_choice_option(item, &format!("{ctx}.items"), &mut item_ids, errors);
    }

    for (k, v) in &step.misconceptions {
        check_id(k, &format!("{ctx}.misconceptions key"), errors);
        check_non_empty_bounded(
            v,
            &format!("{ctx}.misconceptions[{k}]"),
            MAX_SHORT_TEXT_BYTES,
            true,
            errors,
        );
    }

    if let Some(code) = &step.code {
        check_bounded(
            code,
            &format!("{ctx}.code"),
            MAX_CODE_FIELD_BYTES,
            false,
            errors,
        );
    }
    if let Some(tmpl) = &step.template {
        check_bounded(
            tmpl,
            &format!("{ctx}.template"),
            MAX_CODE_FIELD_BYTES,
            false,
            errors,
        );
    }
    if let Some(ph) = &step.placeholder {
        check_bounded(
            ph,
            &format!("{ctx}.placeholder"),
            MAX_SHORT_TEXT_BYTES,
            true,
            errors,
        );
    }
    if let Some(starter) = &step.starter {
        check_bounded(
            starter,
            &format!("{ctx}.starter"),
            MAX_CODE_FIELD_BYTES,
            false,
            errors,
        );
    }

    if let Some(key) = &step.key {
        match key {
            AnswerKey::Choice(ids) => {
                let mut seen = BTreeSet::new();
                for id in ids {
                    if !seen.insert(id.as_str()) {
                        errors.push(format!("{ctx}: duplicate id `{id}` in choice key"));
                    }
                    if !opt_ids.contains(id.as_str()) {
                        errors.push(format!(
                            "{ctx}: choice key references unknown option `{id}`"
                        ));
                    }
                }
            }
            AnswerKey::Order(ids) => {
                let mut seen = BTreeSet::new();
                for id in ids {
                    if !seen.insert(id.as_str()) {
                        errors.push(format!("{ctx}: duplicate id `{id}` in order key"));
                    }
                    if !item_ids.contains(id.as_str()) {
                        errors.push(format!("{ctx}: order key references unknown item `{id}`"));
                    }
                }
            }
            AnswerKey::Text(accepted) => {
                for (i, ans) in accepted.iter().enumerate() {
                    check_non_empty_bounded(
                        ans,
                        &format!("{ctx}.key.text[{i}]"),
                        MAX_SHORT_TEXT_BYTES,
                        false,
                        errors,
                    );
                }
            }
            AnswerKey::Rust { stdout } => {
                check_bounded(
                    stdout,
                    &format!("{ctx}.key.rust.stdout"),
                    MAX_TEXT_BLOCK_BYTES,
                    false,
                    errors,
                );
            }
        }
    }
}

fn validate_blocks(blocks: &[Block], field: &str, errors: &mut Vec<String>) {
    if blocks.len() > MAX_BLOCKS_PER_STEP {
        errors.push(format!(
            "{field}: has {} blocks, exceeding cap of {MAX_BLOCKS_PER_STEP}",
            blocks.len()
        ));
    }
    for (i, block) in blocks.iter().enumerate() {
        let is_prose = matches!(block.kind, BlockKind::P | BlockKind::H | BlockKind::Note);
        check_non_empty_bounded(
            &block.text,
            &format!("{field}[{i}]"),
            MAX_TEXT_BLOCK_BYTES,
            is_prose,
            errors,
        );
        if let Some(lang) = &block.lang {
            check_id(lang, &format!("{field}[{i}].lang"), errors);
        }
    }
}

fn validate_choice_option<'a>(
    opt: &'a ChoiceOption,
    field: &str,
    seen_ids: &mut BTreeSet<&'a str>,
    errors: &mut Vec<String>,
) {
    check_id(&opt.id, &format!("{field}.id"), errors);
    if !seen_ids.insert(opt.id.as_str()) {
        errors.push(format!("{field}: duplicate option/item id `{}`", opt.id));
    }
    check_non_empty_bounded(
        &opt.text,
        &format!("{field}[{}].text", opt.id),
        MAX_SHORT_TEXT_BYTES,
        !opt.code,
        errors,
    );
}

fn validate_fixtures(draft: &LessonDraft, errors: &mut Vec<String>) {
    let find_step = |step_id: &str| -> Option<&Step> {
        draft
            .output
            .lesson
            .steps
            .iter()
            .chain(draft.output.review_variants.iter())
            .find(|s| s.id == step_id)
    };

    if draft.output.rust.len() > MAX_FIXTURES_PER_DRAFT {
        errors.push(format!(
            "draft has {} rust fixtures, exceeding cap of {MAX_FIXTURES_PER_DRAFT}",
            draft.output.rust.len()
        ));
    }
    let mut rust_steps = BTreeSet::new();
    for fix in &draft.output.rust {
        if !rust_steps.insert(fix.step_id.as_str()) {
            errors.push(format!("duplicate RustFixture for step `{}`", fix.step_id));
        }
        match find_step(&fix.step_id) {
            None => errors.push(format!(
                "RustFixture references non-existent step `{}`",
                fix.step_id
            )),
            Some(step) if step.kind != StepKind::RustEdit => errors.push(format!(
                "RustFixture step `{}` has kind `{:?}`, expected `RustEdit`",
                fix.step_id, step.kind
            )),
            Some(_) => {}
        }
    }

    if draft.output.numeric.len() > MAX_FIXTURES_PER_DRAFT {
        errors.push(format!(
            "draft has {} numeric fixtures, exceeding cap of {MAX_FIXTURES_PER_DRAFT}",
            draft.output.numeric.len()
        ));
    }
    let mut num_steps = BTreeSet::new();
    for fix in &draft.output.numeric {
        if !num_steps.insert(fix.step_id.as_str()) {
            errors.push(format!(
                "duplicate NumericFixture for step `{}`",
                fix.step_id
            ));
        }
        match find_step(&fix.step_id) {
            None => errors.push(format!(
                "NumericFixture references non-existent step `{}`",
                fix.step_id
            )),
            Some(step)
                if !matches!(
                    step.kind,
                    StepKind::Fill | StepKind::Choice | StepKind::Predict
                ) =>
            {
                errors.push(format!(
                    "NumericFixture step `{}` has incompatible kind `{:?}`",
                    fix.step_id, step.kind
                ));
            }
            Some(_) => {}
        }
    }

    if draft.output.rubric.len() > MAX_FIXTURES_PER_DRAFT {
        errors.push(format!(
            "draft has {} rubric fixtures, exceeding cap of {MAX_FIXTURES_PER_DRAFT}",
            draft.output.rubric.len()
        ));
    }
    let mut rubric_steps = BTreeSet::new();
    for fix in &draft.output.rubric {
        if !rubric_steps.insert(fix.step_id.as_str()) {
            errors.push(format!(
                "duplicate RubricFixture for step `{}`",
                fix.step_id
            ));
        }
        match find_step(&fix.step_id) {
            None => errors.push(format!(
                "RubricFixture references non-existent step `{}`",
                fix.step_id
            )),
            Some(step) if !step.scored() => errors.push(format!(
                "RubricFixture step `{}` must be a scored step, not `Explain`",
                fix.step_id
            )),
            Some(_) => {}
        }
        for (i, c) in fix.criteria.iter().enumerate() {
            check_non_empty_bounded(
                c,
                &format!("rubric[{}].criteria[{i}]", fix.step_id),
                MAX_SHORT_TEXT_BYTES,
                true,
                errors,
            );
        }
        for (i, np) in fix.not_penalised.iter().enumerate() {
            check_non_empty_bounded(
                np,
                &format!("rubric[{}].not_penalised[{i}]", fix.step_id),
                MAX_SHORT_TEXT_BYTES,
                true,
                errors,
            );
        }
    }
}

fn check_id(val: &str, field: &str, errors: &mut Vec<String>) {
    if val.trim().is_empty() {
        errors.push(format!("{field} cannot be empty"));
        return;
    }
    if val.len() > MAX_ID_BYTES {
        errors.push(format!(
            "{field} length {} exceeds {MAX_ID_BYTES} bytes",
            val.len()
        ));
    }
    if let Some(reason) = detect_executable_markup(val, true) {
        errors.push(format!("{field} contains forbidden markup: {reason}"));
    }
}

fn check_non_empty_bounded(
    val: &str,
    field: &str,
    max_bytes: usize,
    is_prose: bool,
    errors: &mut Vec<String>,
) {
    if val.trim().is_empty() {
        errors.push(format!("{field} cannot be empty"));
        return;
    }
    check_bounded(val, field, max_bytes, is_prose, errors);
}

fn check_bounded(
    val: &str,
    field: &str,
    max_bytes: usize,
    is_prose: bool,
    errors: &mut Vec<String>,
) {
    if val.len() > max_bytes {
        errors.push(format!(
            "{field} length {} exceeds cap of {max_bytes} bytes",
            val.len()
        ));
    }
    if let Some(reason) = detect_executable_markup(val, is_prose) {
        errors.push(format!("{field} contains forbidden markup: {reason}"));
    }
}

/// Scans `text` for executable markup (`<script`, `<iframe`, `javascript:`, `data:` URLs,
/// `on*=` handlers) and, when `reject_raw_html` is true (`p`/`h`/`note` blocks and titles),
/// raw HTML tags.
pub fn detect_executable_markup(text: &str, reject_raw_html: bool) -> Option<String> {
    let lower = text.to_ascii_lowercase();

    for pattern in [
        "<script",
        "</script",
        "<iframe",
        "</iframe",
        "<object",
        "<embed",
        "javascript:",
        "vbscript:",
    ] {
        if lower.contains(pattern) {
            return Some(format!("matched `{pattern}`"));
        }
    }

    if contains_data_url(&lower) {
        return Some("matched `data:` URL".to_string());
    }

    if contains_inline_event_handler(&lower) {
        return Some("matched inline `on*=` event handler".to_string());
    }

    if reject_raw_html && let Some(tag) = detect_raw_html_tag(&lower) {
        return Some(format!("raw HTML tag `<{tag}>` in prose block"));
    }

    None
}

fn contains_data_url(lower: &str) -> bool {
    let bytes = lower.as_bytes();
    let needle = b"data:";
    if bytes.len() < needle.len() {
        return false;
    }
    for i in 0..=bytes.len() - needle.len() {
        if &bytes[i..i + needle.len()] == needle {
            // Check boundary before `data:`: start of string or non-alphanumeric/`_`/`:`.
            if i > 0 {
                let prev = bytes[i - 1];
                if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b':' {
                    continue;
                }
            }
            let rest = &lower[i + needle.len()..];
            // In Rust code `data: Vec<u8>` has a space after `:`. A `data:` URL is followed
            // immediately by MIME type, `,`, or `;base64`.
            if let Some(first_char) = rest.chars().next()
                && !first_char.is_whitespace()
                && (rest.starts_with("text/")
                    || rest.starts_with("image/")
                    || rest.starts_with("application/")
                    || rest.starts_with("audio/")
                    || rest.starts_with("video/")
                    || rest.starts_with("font/")
                    || rest.starts_with(',')
                    || rest.starts_with(";base64"))
            {
                return true;
            }
        }
    }
    false
}

fn contains_inline_event_handler(lower: &str) -> bool {
    const HANDLERS: &[&str] = &[
        "onerror",
        "onload",
        "onclick",
        "ondblclick",
        "onmouseover",
        "onmouseout",
        "onmouseenter",
        "onmouseleave",
        "onmousedown",
        "onmouseup",
        "onfocus",
        "onblur",
        "onchange",
        "onsubmit",
        "onreset",
        "onkeydown",
        "onkeyup",
        "onkeypress",
        "oninput",
        "onanimationstart",
        "ontoggle",
        "onwheel",
        "onpaste",
        "oncopy",
        "oncut",
    ];
    for handler in HANDLERS {
        let mut search_from = 0;
        while let Some(pos) = lower[search_from..].find(handler) {
            let abs = search_from + pos;
            let before_ok = abs == 0 || {
                let b = lower.as_bytes()[abs - 1];
                !b.is_ascii_alphanumeric() && b != b'_'
            };
            let after = &lower[abs + handler.len()..];
            let after_trimmed = after.trim_start();
            if before_ok && after_trimmed.starts_with('=') {
                return true;
            }
            search_from = abs + handler.len();
        }
    }
    false
}

fn detect_raw_html_tag(lower: &str) -> Option<String> {
    const HTML_TAGS: &[&str] = &[
        "a",
        "abbr",
        "address",
        "area",
        "article",
        "aside",
        "audio",
        "b",
        "base",
        "bdi",
        "bdo",
        "blockquote",
        "body",
        "br",
        "button",
        "canvas",
        "caption",
        "cite",
        "code",
        "col",
        "colgroup",
        "data",
        "datalist",
        "dd",
        "del",
        "details",
        "dfn",
        "dialog",
        "div",
        "dl",
        "dt",
        "em",
        "embed",
        "fieldset",
        "figcaption",
        "figure",
        "footer",
        "form",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "head",
        "header",
        "hgroup",
        "hr",
        "html",
        "i",
        "iframe",
        "img",
        "input",
        "ins",
        "kbd",
        "label",
        "legend",
        "li",
        "link",
        "main",
        "map",
        "mark",
        "math",
        "menu",
        "meta",
        "meter",
        "nav",
        "noscript",
        "object",
        "ol",
        "optgroup",
        "option",
        "output",
        "p",
        "param",
        "picture",
        "pre",
        "progress",
        "q",
        "rp",
        "rt",
        "ruby",
        "s",
        "samp",
        "script",
        "search",
        "section",
        "select",
        "slot",
        "small",
        "source",
        "span",
        "strong",
        "style",
        "sub",
        "summary",
        "sup",
        "svg",
        "table",
        "tbody",
        "td",
        "template",
        "textarea",
        "tfoot",
        "th",
        "thead",
        "time",
        "title",
        "tr",
        "track",
        "u",
        "ul",
        "var",
        "video",
        "wbr",
    ];

    let bytes = lower.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let rest = &lower[i + 1..];
            if rest.starts_with("!--")
                || rest.starts_with("!doctype")
                || rest.starts_with("![cdata[")
            {
                return Some("!--".to_string());
            }
            let (is_closing, after_slash) = if let Some(stripped) = rest.strip_prefix('/') {
                (true, stripped)
            } else {
                (false, rest)
            };
            let name_len = after_slash
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric() || *b == b'-')
                .count();
            if name_len > 0 {
                let tag_name = &after_slash[..name_len];
                let after_name = &after_slash[name_len..];
                if let Some(close_pos) = after_name.find('>') {
                    let inside = &after_name[..close_pos];
                    let valid_boundary = inside.is_empty()
                        || inside == "/"
                        || inside.starts_with(char::is_whitespace);
                    if valid_boundary
                        && (is_closing || HTML_TAGS.contains(&tag_name) || inside.contains('='))
                    {
                        return Some(tag_name.to_string());
                    }
                }
            }
        }
        i += 1;
    }
    None
}

fn synthetic_review_for_objective(objective_id: &str, idx: usize) -> Step {
    Step {
        id: format!("__schema_synth_rev_{idx}"),
        kind: StepKind::Choice,
        family: Some(format!("__schema_synth_fam_{idx}")),
        title: "Synthetic review placeholder".to_string(),
        objective_ids: vec![objective_id.to_string()],
        confidence: false,
        body: vec![Block::p("Synthetic review step for schema check.")],
        options: vec![
            ChoiceOption {
                id: "a".to_string(),
                text: "Option A".to_string(),
                code: false,
            },
            ChoiceOption {
                id: "b".to_string(),
                text: "Option B".to_string(),
                code: false,
            },
        ],
        multi: false,
        code: None,
        items: Vec::new(),
        template: None,
        placeholder: None,
        starter: None,
        limits: None,
        key: Some(AnswerKey::Choice(vec!["a".to_string()])),
        misconceptions: Default::default(),
        hints: Vec::new(),
        correct: Vec::new(),
        solution: Vec::new(),
    }
}
