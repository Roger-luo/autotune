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
