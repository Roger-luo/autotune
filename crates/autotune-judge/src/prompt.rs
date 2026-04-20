use crate::model::{Rubric, StoredExample, Subject};

/// Render the assessment prompt. The caller's contract with the model is:
/// the response must be exactly two lines in the form `score: <int>\nreason: <sentence>`.
pub fn render_assessment_prompt(
    subject: &Subject,
    rubric: &Rubric,
    examples: &[StoredExample],
) -> String {
    let example_block = if examples.is_empty() {
        String::new()
    } else {
        examples
            .iter()
            .map(|ex| {
                format!(
                    "Example rubric: {}\nApproved score: {}\nApproved reason: {}",
                    ex.rubric.title, ex.review.approved_score, ex.review.approved_reason
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    };

    let context_block = subject.render_context();

    let guidance_block = match &rubric.guidance {
        Some(g) if !g.trim().is_empty() => format!("Guidance: {g}\n"),
        _ => String::new(),
    };

    format!(
        "You are judging from this persona: {persona}\n\
         Rubric: {title}\n\
         Instruction: {instruction}\n\
         {guidance}\
         Score range: {min} to {max}\n\
         Subject title: {subject_title}\n\
         Subject summary: {subject_summary}\n\
         Additional context:\n{context}\n\
         {examples}\n\
         Return exactly two lines:\n\
         score: <integer>\n\
         reason: <one sentence>\n",
        persona = rubric.persona,
        title = rubric.title,
        instruction = rubric.instruction,
        guidance = guidance_block,
        min = rubric.score_range.min,
        max = rubric.score_range.max,
        subject_title = subject.title,
        subject_summary = subject.summary,
        context = context_block,
        examples = example_block,
    )
}

/// Render a batched assessment prompt for multiple rubrics under a shared persona.
///
/// The agent must return one blank-line-separated block per rubric:
/// ```text
/// <rubric-id>
/// score: <int>
/// reason: <one sentence>
/// ```
pub fn render_batch_prompt(persona: &str, subject: &Subject, rubrics: &[Rubric]) -> String {
    let context_block = subject.render_context();

    let rubric_list = rubrics
        .iter()
        .map(|r| {
            let guidance = match &r.guidance {
                Some(g) if !g.trim().is_empty() => format!("Guidance: {g}\n"),
                _ => String::new(),
            };
            format!(
                "## {id} — {title} (score {min} to {max})\n{instruction}\n{guidance}",
                id = r.id,
                title = r.title,
                min = r.score_range.min,
                max = r.score_range.max,
                instruction = r.instruction,
                guidance = guidance,
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let ids_list = rubrics
        .iter()
        .map(|r| {
            format!(
                "{id}\nscore: <integer between {min} and {max}>\nreason: <one sentence>",
                id = r.id,
                min = r.score_range.min,
                max = r.score_range.max,
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    format!(
        "You are judging as: {persona}\n\n\
         Subject: {title}\n\
         Summary: {summary}\n\
         Context:\n{context}\n\n\
         Score every rubric below. Return exactly one block per rubric ID, \
         separated by a blank line, in any order. Each block must be:\n\
         <rubric-id>\n\
         score: <integer>\n\
         reason: <one sentence>\n\n\
         Required response shape:\n\
         {ids_list}\n\n\
         Rubrics:\n\
         {rubric_list}",
        persona = persona,
        title = subject.title,
        summary = subject.summary,
        context = context_block,
        ids_list = ids_list,
        rubric_list = rubric_list,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Rubric, ScoreRange, Subject};

    fn make_rubric(id: &str, min: i32, max: i32) -> Rubric {
        Rubric {
            id: id.to_string(),
            title: format!("{id} title"),
            persona: "shared".to_string(),
            score_range: ScoreRange { min, max },
            instruction: format!("Score {id} from {min} to {max}."),
            guidance: None,
        }
    }

    #[test]
    fn batch_prompt_contains_persona_and_all_rubric_ids() {
        let subject = Subject::new("my-subject", "approach-alpha");
        let rubrics = vec![make_rubric("r1", 1, 5), make_rubric("r2", 1, 3)];
        let prompt = render_batch_prompt("A strict expert", &subject, &rubrics);
        assert!(prompt.contains("A strict expert"));
        assert!(prompt.contains("r1"));
        assert!(prompt.contains("r2"));
        assert!(prompt.contains("r1 title"));
        assert!(prompt.contains("r2 title"));
        assert!(prompt.contains("score: <integer>"));
        assert!(prompt.contains("reason: <one sentence>"));
    }

    #[test]
    fn batch_prompt_includes_guidance_when_present() {
        let subject = Subject::new("s", "a");
        let mut rubric = make_rubric("r1", 1, 5);
        rubric.guidance = Some("Check edge cases.".to_string());
        let prompt = render_batch_prompt("Reviewer", &subject, &[rubric]);
        assert!(prompt.contains("Check edge cases."));
    }

    #[test]
    fn batch_prompt_includes_subject_context() {
        use crate::model::{SubjectContext, SubjectContextKind};
        let mut subject = Subject::new("title", "summary");
        subject = subject.with_context(vec![SubjectContext {
            kind: SubjectContextKind::Note,
            label: "iteration".to_string(),
            body: "3".to_string(),
        }]);
        let prompt = render_batch_prompt("P", &subject, &[make_rubric("r1", 1, 5)]);
        assert!(prompt.contains("iteration"));
        assert!(prompt.contains("3"));
    }
}
