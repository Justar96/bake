//! The terminal's side of `ask_user`: the oldest question request of the
//! session's [`QuestionBroker`] is put to the person as a run of dialogs,
//! one per question, and its answers settle the request together.
//!
//! A question with options is a list: one row per option, with its
//! description at the right, and a last row to type an answer instead. A
//! multiple-choice question shows a box before each option; choosing a row
//! ticks or clears it, and `Done` answers with what is ticked. A question
//! without options is a one-line field. Esc on any of them declines the
//! whole request. Questions a spawned agent forwards name it in the title.
//!
//! The dialogs are the view's own ([`bake_tui_view::dialog`]), so their
//! keyboard handover protects a question as it protects a command's
//! prompt. Their ids come from the counter commands use, and answers reach
//! the desk as they reach a command: the desk drops those for other ids.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, PoisonError};

use bake_coding_agent::questions::{Question, QuestionAnswer, QuestionBroker, QuestionPrompt};
use bake_tui_view::dialog::{
    DialogAnswer, DialogKind, DialogRequest, FieldDialog, ListDialog, ListItem,
};
use bake_tui_view::runtime::RuntimeUpdate;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use super::Send;
use super::commands::next_dialog_id;

/// The value of the row that types an answer instead.
const OTHER: &str = "\u{0}other";
/// The value of a multiple-choice question's `Done` row.
const DONE: &str = "\u{0}done";

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
        let title = title(question, prompt.from.as_deref(), index, count);
        let entry = match question.options.is_empty() {
            true => typed(&title, &question.question, &send, &ids, &mut inbox).await,
            false if question.multi_select => {
                several(&title, question, &send, &ids, &mut inbox).await
            }
            false => one(&title, question, &send, &ids, &mut inbox).await,
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

fn list(title: &str, note: &str, items: Vec<ListItem>, selected: Option<usize>) -> DialogKind {
    DialogKind::List(ListDialog {
        title: title.to_owned(),
        items,
        current: None,
        selected,
        filterable: false,
        saves_default: false,
        note: Some(note.to_owned()),
        search: None,
    })
}

fn item(value: &str, label: String, detail: &str) -> ListItem {
    ListItem {
        value: value.to_owned(),
        label,
        detail: (!detail.is_empty()).then(|| detail.to_owned()),
        ..ListItem::default()
    }
}

fn other_item() -> ListItem {
    item(OTHER, "Type my own answer".to_owned(), "")
}

async fn one(
    title: &str,
    question: &Question,
    send: &Send,
    ids: &AtomicU64,
    inbox: &mut mpsc::UnboundedReceiver<(u64, DialogAnswer)>,
) -> Option<Vec<String>> {
    let mut items: Vec<ListItem> = question
        .options
        .iter()
        .map(|option| item(&option.label, option.label.clone(), &option.description))
        .collect();
    items.push(other_item());
    loop {
        let kind = list(title, &question.question, items.clone(), None);
        match open(kind, send, ids, inbox).await {
            DialogAnswer::Chose { value, .. } if value == OTHER => {
                if let Some(typed) = typed(title, &question.question, send, ids, inbox).await {
                    return Some(typed);
                }
                // Esc on the field goes back to the options.
            }
            DialogAnswer::Chose { value, .. } => return Some(vec![value]),
            _ => return None,
        }
    }
}

async fn several(
    title: &str,
    question: &Question,
    send: &Send,
    ids: &AtomicU64,
    inbox: &mut mpsc::UnboundedReceiver<(u64, DialogAnswer)>,
) -> Option<Vec<String>> {
    let mut ticked = vec![false; question.options.len()];
    let mut selected = None;
    loop {
        let chosen = ticked.iter().filter(|tick| **tick).count();
        let mut items: Vec<ListItem> = question
            .options
            .iter()
            .zip(&ticked)
            .map(|(option, tick)| {
                let mark = if *tick { "[x]" } else { "[ ]" };
                item(
                    &option.label,
                    format!("{mark} {}", option.label),
                    &option.description,
                )
            })
            .collect();
        items.push(other_item());
        items.push(item(DONE, format!("Done ({chosen} chosen)"), ""));
        let kind = list(title, &question.question, items, selected);
        match open(kind, send, ids, inbox).await {
            DialogAnswer::Chose { value, .. } if value == DONE => {
                return Some(
                    question
                        .options
                        .iter()
                        .zip(&ticked)
                        .filter(|(_, tick)| **tick)
                        .map(|(option, _)| option.label.clone())
                        .collect(),
                );
            }
            DialogAnswer::Chose { value, .. } if value == OTHER => {
                if let Some(mut typed) = typed(title, &question.question, send, ids, inbox).await {
                    let mut entry: Vec<String> = question
                        .options
                        .iter()
                        .zip(&ticked)
                        .filter(|(_, tick)| **tick)
                        .map(|(option, _)| option.label.clone())
                        .collect();
                    entry.append(&mut typed);
                    return Some(entry);
                }
                selected = Some(question.options.len());
            }
            DialogAnswer::Chose { value, .. } => {
                if let Some(index) = question.options.iter().position(|o| o.label == value) {
                    ticked[index] = !ticked[index];
                    selected = Some(index);
                }
            }
            _ => return None,
        }
    }
}

async fn typed(
    title: &str,
    prompt: &str,
    send: &Send,
    ids: &AtomicU64,
    inbox: &mut mpsc::UnboundedReceiver<(u64, DialogAnswer)>,
) -> Option<Vec<String>> {
    let kind = DialogKind::Field(Box::new(FieldDialog {
        title: title.to_owned(),
        prompt: prompt.to_owned(),
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

    fn choose(value: &'static str) -> Step {
        Box::new(move |_| DialogAnswer::Chose {
            value: value.to_owned(),
            save_default: false,
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
                choose("a"),
                choose("a"),
                choose("c"),
                choose(DONE),
                choose("y"),
            ],
        )
        .await;
        assert_eq!(
            answer,
            QuestionAnswer::Answered(vec![vec!["c".into()], vec!["y".into()]])
        );
        let DialogKind::List(last_multi) = &shown[3] else {
            panic!()
        };
        assert_eq!(last_multi.items[2].label, "[x] c");
        assert_eq!(last_multi.items[4].label, "Done (1 chosen)");
        assert_eq!(last_multi.selected, Some(2));
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
