//! The tools the Folio shows as something other than a step: the questions
//! of AskUserQuestion and the todo list of TodoWrite.

use serde_json::Value;

/// One question of an AskUserQuestion call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Question {
    pub question: String,
    /// A short label for it, such as "Color".
    pub header: String,
    pub options: Vec<QuestionOption>,
    /// More than one option may be picked.
    pub multi_select: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

/// The questions in an AskUserQuestion input.
pub fn questions(input: &Value) -> Vec<Question> {
    let s = |v: &Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
    input["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|q| Question {
            question: s(q, "question"),
            header: s(q, "header"),
            multi_select: q["multiSelect"].as_bool().unwrap_or(false),
            options: q["options"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|o| QuestionOption {
                    label: s(o, "label"),
                    description: s(o, "description"),
                })
                .collect(),
        })
        .collect()
}

/// The answers in the form the CLI takes them: by question text, several
/// picks of one question joined with ", " as the terminal joins them.
pub fn answers_object(questions: &[Question], picks: &[Vec<String>]) -> Value {
    let mut map = serde_json::Map::new();
    for (q, p) in questions.iter().zip(picks) {
        map.insert(q.question.clone(), Value::String(p.join(", ")));
    }
    Value::Object(map)
}

/// One item of the todo list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Todo {
    /// What to do, such as "Run the tests".
    pub content: String,
    /// The same while it is being done, such as "Running the tests".
    pub active_form: String,
    pub status: TodoStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

/// The list in a TodoWrite input, which is the whole list each time. The
/// model sometimes sends it as a JSON string rather than an array.
pub fn todos(input: &Value) -> Option<Vec<Todo>> {
    let parsed;
    let list = match &input["todos"] {
        Value::String(s) => {
            parsed = serde_json::from_str::<Value>(s).ok()?;
            parsed.as_array()?
        }
        v => v.as_array()?,
    };
    let s = |v: &Value, k: &str| v[k].as_str().unwrap_or_default().to_string();
    Some(
        list.iter()
            .map(|t| Todo {
                content: s(t, "content"),
                active_form: s(t, "activeForm"),
                status: match t["status"].as_str().unwrap_or_default() {
                    "in_progress" => TodoStatus::InProgress,
                    "completed" => TodoStatus::Completed,
                    _ => TodoStatus::Pending,
                },
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn questions_read_their_options() {
        let q = questions(&json!({"questions": [{"question": "Which?", "header": "H",
            "multiSelect": true, "options": [{"label": "A", "description": "a"}]}]}));
        assert_eq!(q[0].question, "Which?");
        assert!(q[0].multi_select);
        assert_eq!(q[0].options[0].label, "A");
        assert_eq!(
            answers_object(&q, &[vec!["A".into(), "B".into()]]),
            json!({"Which?": "A, B"})
        );
    }

    #[test]
    fn todos_read_as_an_array_or_a_string() {
        let list = json!([{"content": "say hi", "status": "completed", "activeForm": "Saying hi"},
            {"content": "say bye", "status": "in_progress", "activeForm": "Saying bye"}]);
        let a = todos(&json!({"todos": list})).unwrap();
        let b = todos(&json!({"todos": list.to_string()})).unwrap();
        assert_eq!(a, b);
        assert_eq!(a[0].status, TodoStatus::Completed);
        assert_eq!(a[1].active_form, "Saying bye");
        assert!(todos(&json!({})).is_none());
    }
}
