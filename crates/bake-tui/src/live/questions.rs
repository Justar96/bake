//! The terminal's side of `ask_user`: the oldest question request of the
//! session's [`QuestionBroker`] is put to the person as a run of dialogs,
//! one per question, and its answers settle the request together.
//!
//! A question with options is a [`QuestionDialog`]: numbered rows with
//! labels and wrapped descriptions, an `Other` row to type an answer instead,
//! and in multiple-choice mode aligned `[x]`/`[ ]` checkboxes and a `Done`
//! row. A question without options is a one-line field. Esc on any of them
//! declines the whole request. Questions a spawned agent forwards name it in
//! the title.
//!
//! The dialogs are the view's own ([`bake_tui_view::dialog`]), so their
//! keyboard handover protects a question as it protects a command's
//! prompt. Their ids come from the counter commands use, and answers reach
//! the desk as they reach a command: the desk drops those for other ids.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, PoisonError};

use bake_coding_agent::questions::{Question, QuestionAnswer, QuestionBroker, QuestionPrompt};
use bake_tui_view::dialog::{
    DialogAnswer, DialogKind, DialogRequest, FieldDialog, QuestionChoice, QuestionDialog,
};
use bake_tui_view::runtime::RuntimeUpdate;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::Send;
use super::commands::next_dialog_id;

/// One request being asked.
struct Asking {
    generation: u64,
    task: JoinHandle<()>,
    answers: mpsc::UnboundedSender<(u64, DialogAnswer)>,
}

/// Puts the broker's questions to the person.
pub(super) struct Desk {
    asking: Mutex<Option<Asking>>,
    send: Send,
    ids: Arc<AtomicU64>,
    runtime: tokio::runtime::Handle,
}

impl Desk {
    /// Connects to `broker`. Returns `None` when another frontend has.
    pub(super) fn connect(
        broker: &QuestionBroker,
        send: Send,
        ids: Arc<AtomicU64>,
        runtime: tokio::runtime::Handle,
    ) -> Option<Arc<Self>> {
        let desk = Arc::new(Self {
            asking: Mutex::new(None),
            send,
            ids,
            runtime,
        });
        let listener = {
            let desk = Arc::downgrade(&desk);
            let broker = broker.clone();
            move |queue: Vec<QuestionPrompt>| match desk.upgrade() {
                Some(desk) => {
                    desk.show(&broker, queue.into_iter().next());
                    true
                }
                None => false,
            }
        };
        broker.connect(Box::new(listener)).then_some(desk)
    }

    /// Starts asking `front` unless it is already being asked; stops
    /// asking a request that left the queue.
    fn show(&self, broker: &QuestionBroker, front: Option<QuestionPrompt>) {
        let mut asking = self.asking.lock().unwrap_or_else(PoisonError::into_inner);
        if let (Some(current), Some(front)) = (asking.as_ref(), front.as_ref())
            && current.generation == front.generation
        {
            return;
        }
        if let Some(previous) = asking.take() {
            previous.task.abort();
            (self.send)(RuntimeUpdate::Dialog(None));
        }
        let Some(front) = front else { return };
        let (answers, inbox) = mpsc::unbounded_channel();
        let generation = front.generation;
        let task = self.runtime.spawn(ask(
            broker.clone(),
            front,
            Arc::clone(&self.send),
            Arc::clone(&self.ids),
            inbox,
        ));
        *asking = Some(Asking {
            generation,
            task,
            answers,
        });
    }

    /// Passes a dialog's answer on; the request's task drops answers for
    /// dialogs it did not open.
    pub(super) fn answer(&self, id: u64, answer: &DialogAnswer) {
        if let Some(asking) = self
            .asking
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
        {
            let _ = asking.answers.send((id, answer.clone()));
        }
    }

    /// Stops asking, as the session ends.
    pub(super) fn close(&self) {
        if let Some(asking) = self
            .asking
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            asking.task.abort();
        }
    }
}

/// Opens `kind` and waits for its answer.
async fn open(
    kind: DialogKind,
    send: &Send,
    ids: &AtomicU64,
    inbox: &mut mpsc::UnboundedReceiver<(u64, DialogAnswer)>,
) -> DialogAnswer {
    let Some(id) = next_dialog_id(ids) else {
        return DialogAnswer::Cancel;
    };
    if !send(RuntimeUpdate::Dialog(Some(DialogRequest { id, kind }))) {
        return DialogAnswer::Cancel;
    }
    loop {
        match inbox.recv().await {
            Some((answered, answer)) if answered == id => return answer,
            Some(_) => {}
            None => return DialogAnswer::Cancel,
        }
    }
}

async fn ask(
    broker: QuestionBroker,
    prompt: QuestionPrompt,
    send: Send,
    ids: Arc<AtomicU64>,
    mut inbox: mpsc::UnboundedReceiver<(u64, DialogAnswer)>,
) {
    let count = prompt.questions.len();
    let mut entries = Vec::with_capacity(count);
    for (index, question) in prompt.questions.iter().enumerate() {
        let entry = match question.options.is_empty() {
            true => {
                let field_title = title(question, prompt.from.as_deref(), index, count);
                typed(
                    &field_title,
                    &question.question,
                    false,
                    &send,
                    &ids,
                    &mut inbox,
                )
                .await
            }
            false if question.multi_select => {
                several(
                    question,
                    prompt.from.as_deref(),
                    index,
                    count,
                    &send,
                    &ids,
                    &mut inbox,
                )
                .await
            }
            false => {
                one(
                    question,
                    prompt.from.as_deref(),
                    index,
                    count,
                    &send,
                    &ids,
                    &mut inbox,
                )
                .await
            }
        };
        match entry {
            Some(entry) => entries.push(entry),
            None => {
                send(RuntimeUpdate::Dialog(None));
                broker.answer(prompt.generation, QuestionAnswer::Declined);
                return;
            }
        }
    }
    send(RuntimeUpdate::Dialog(None));
    broker.answer(prompt.generation, QuestionAnswer::Answered(entries));
}

/// `Database · 1 of 2`, `alpha-1 asks · Database`.
pub(super) fn title(question: &Question, from: Option<&str>, index: usize, count: usize) -> String {
    let mut title = if question.header.is_empty() {
        "Question".to_owned()
    } else {
        question.header.clone()
    };
    if let Some(from) = from {
        title = format!("{from} asks · {title}");
    }
    if count > 1 {
        title = format!("{title} · {} of {count}", index + 1);
    }
    title
}

fn question_dialog(
    question: &Question,
    from: Option<&str>,
    index: usize,
    count: usize,
    ticked: Vec<bool>,
    selected: Option<usize>,
) -> DialogKind {
    DialogKind::Question(Box::new(QuestionDialog {
        header: question.header.clone(),
        from: from.map(str::to_owned),
        progress: (count > 1).then_some((index + 1, count)),
        question: question.question.clone(),
        options: question
            .options
            .iter()
            .map(|option| QuestionChoice {
                value: option.label.clone(),
                label: option.label.clone(),
                description: (!option.description.is_empty()).then(|| option.description.clone()),
            })
            .collect(),
        multi_select: question.multi_select,
        ticked,
        selected,
    }))
}

async fn one(
    question: &Question,
    from: Option<&str>,
    index: usize,
    count: usize,
    send: &Send,
    ids: &AtomicU64,
    inbox: &mut mpsc::UnboundedReceiver<(u64, DialogAnswer)>,
) -> Option<Vec<String>> {
    let field_title = title(question, from, index, count);
    let mut selected = None;
    loop {
        let kind = question_dialog(question, from, index, count, Vec::new(), selected);
        match open(kind, send, ids, inbox).await {
            DialogAnswer::Question { other: true, .. } => {
                if let Some(typed) =
                    typed(&field_title, &question.question, true, send, ids, inbox).await
                {
                    return Some(typed);
                }
                // Esc on the field goes back to the options.
                selected = Some(question.options.len());
            }
            DialogAnswer::Question {
                values,
                other: false,
            } => return Some(values),
            _ => return None,
        }
    }
}

async fn several(
    question: &Question,
    from: Option<&str>,
    index: usize,
    count: usize,
    send: &Send,
    ids: &AtomicU64,
    inbox: &mut mpsc::UnboundedReceiver<(u64, DialogAnswer)>,
) -> Option<Vec<String>> {
    let field_title = title(question, from, index, count);
    let mut ticked = vec![false; question.options.len()];
    let mut selected = None;
    loop {
        let kind = question_dialog(question, from, index, count, ticked.clone(), selected);
        match open(kind, send, ids, inbox).await {
            DialogAnswer::Question {
                values,
                other: false,
            } => return Some(values),
            DialogAnswer::Question {
                values,
                other: true,
            } => {
                ticked = question
                    .options
                    .iter()
                    .map(|option| values.contains(&option.label))
                    .collect();
                if let Some(mut typed) =
                    typed(&field_title, &question.question, true, send, ids, inbox).await
                {
                    let mut entry = values;
                    entry.append(&mut typed);
                    return Some(entry);
                }
                selected = Some(question.options.len());
            }
            _ => return None,
        }
    }
}

async fn typed(
    title: &str,
    prompt: &str,
    back_to_options: bool,
    send: &Send,
    ids: &AtomicU64,
    inbox: &mut mpsc::UnboundedReceiver<(u64, DialogAnswer)>,
) -> Option<Vec<String>> {
    let kind = DialogKind::Field(Box::new(FieldDialog {
        title: title.to_owned(),
        prompt: prompt.to_owned(),
        placeholder: Some("Type your answer…".to_owned()),
        hint: back_to_options.then(|| "Esc goes back to the options".to_owned()),
        action: Some("answer".to_owned()),
        ..FieldDialog::default()
    }));
    match open(kind, send, ids, inbox).await {
        DialogAnswer::Entered(text) => {
            let text = text.expose().trim().to_owned();
            Some(if text.is_empty() {
                Vec::new()
            } else {
                vec![text]
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bake_coding_agent::questions::QuestionOption;

    fn question(options: &[&str], multi: bool) -> Question {
        Question {
            question: "Which?".into(),
            header: "Pick".into(),
            options: options
                .iter()
                .map(|label| QuestionOption {
                    label: (*label).into(),
                    description: String::new(),
                })
                .collect(),
            multi_select: multi,
        }
    }

    #[test]
    fn titles_name_who_asks_and_where_in_the_run() {
        let q = question(&["a", "b"], false);
        assert_eq!(title(&q, None, 0, 1), "Pick");
        assert_eq!(
            title(&q, Some("alpha-1"), 1, 2),
            "alpha-1 asks · Pick · 2 of 2"
        );
    }

    /// One answer the person gives a dialog.
    type Step = Box<dyn Fn(&DialogKind) -> DialogAnswer + std::marker::Send>;

    /// Plays the person: answers each dialog the desk opens with the next
    /// of `script`, and returns the request's answer.
    async fn play(
        questions: Vec<Question>,
        script: Vec<Step>,
    ) -> (QuestionAnswer, Vec<DialogKind>) {
        let broker = QuestionBroker::new();
        let (updates, mut seen) = mpsc::unbounded_channel();
        let send: Send = Arc::new(move |update| updates.send(update).is_ok());
        let desk = Desk::connect(
            &broker,
            send,
            Arc::default(),
            tokio::runtime::Handle::current(),
        )
        .unwrap();
        let asking = tokio::spawn({
            let broker = broker.clone();
            async move { broker.ask("c1", None, questions, None).await }
        });
        let mut shown = Vec::new();
        let mut script = script.into_iter();
        while let Some(update) = seen.recv().await {
            match update {
                RuntimeUpdate::Dialog(Some(request)) => {
                    let answer = (script.next().expect("a scripted answer"))(&request.kind);
                    shown.push(request.kind);
                    desk.answer(request.id, &answer);
                }
                RuntimeUpdate::Dialog(None) => break,
                _ => {}
            }
        }
        (asking.await.unwrap().unwrap(), shown)
    }

    fn answer_values(values: &'static [&'static str], other: bool) -> Step {
        Box::new(move |_| DialogAnswer::Question {
            values: values.iter().map(|v| (*v).to_owned()).collect(),
            other,
        })
    }

    #[tokio::test]
    async fn a_run_of_questions_is_answered_together() {
        let (answer, shown) = play(
            vec![
                question(&["a", "b", "c"], true),
                question(&["x", "y"], false),
            ],
            vec![
                answer_values(&["c"], true),
                Box::new(|_| DialogAnswer::Cancel),
                answer_values(&["c"], false),
                answer_values(&["y"], false),
            ],
        )
        .await;
        assert_eq!(
            answer,
            QuestionAnswer::Answered(vec![vec!["c".into()], vec!["y".into()]])
        );
        let DialogKind::Question(reopened) = &shown[2] else {
            panic!()
        };
        assert_eq!(reopened.ticked, vec![false, false, true]);
        assert_eq!(reopened.selected, Some(3));
        assert_eq!(reopened.progress, Some((1, 2)));
        let DialogKind::Question(second) = &shown[3] else {
            panic!()
        };
        assert_eq!(second.progress, Some((2, 2)));
        assert!(!second.multi_select);
    }

    #[tokio::test]
    async fn esc_declines_and_typing_answers() {
        let (answer, _) = play(
            vec![question(&["a", "b"], false)],
            vec![Box::new(|_| DialogAnswer::Cancel)],
        )
        .await;
        assert_eq!(answer, QuestionAnswer::Declined);
    }
}
