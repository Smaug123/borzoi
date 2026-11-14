//! The protocol between the input thread and the loop, over every
//! interleaving.
//!
//! The input thread's actions are those [`handover`] describes for each
//! notification, which [`InboxSender::send`] performs, in order. The loop's are
//! [`Inbox::begin_step_interleaved`] (the counter, then each waiting message),
//! one notification applied per step, as the plan applies them, and, once no
//! notification is waiting, a request's run: two checkpoints, then the one
//! after the handler, then its answer. A run stopped at a checkpoint is run
//! again in a later step. The input thread may act at any point between two
//! of the loop's actions; the explorer below tries every way of spreading its
//! actions over those points.

use std::cell::RefCell;
use std::collections::VecDeque;

use lsp_server::{Message, Notification};

use super::*;

/// Checkpoints a run passes before the one after its handler.
const CHECKPOINTS: usize = 2;

/// The input thread's actions for `notifications` notifications, as
/// [`handover`] describes them, each with the notification it belongs to.
struct Input {
    sender: InboxSender,
    actions: VecDeque<(usize, Handover)>,
    /// For each notification, whether it has been counted.
    counted: Vec<bool>,
}

impl Input {
    fn new(sender: InboxSender, notifications: usize) -> Input {
        let actions = (0..notifications)
            .flat_map(|i| {
                handover(Message::Notification(Notification {
                    method: format!("n{i}"),
                    params: serde_json::Value::Null,
                }))
                .into_iter()
                .map(move |action| (i, action))
            })
            .collect();
        Input {
            sender,
            actions,
            counted: vec![false; notifications],
        }
    }

    fn remaining(&self) -> usize {
        self.actions.len()
    }

    fn act(&mut self) {
        let (i, action) = self.actions.pop_front().expect("an action remains");
        if matches!(action, Handover::Count) {
            self.counted[i] = true;
        }
        self.sender.perform(action).expect("the inbox is alive");
    }
}

/// Replays a prefix of choices and extends it with zeros, recording how many
/// choices each point had, so that every schedule is visited once.
struct Explorer {
    choices: Vec<usize>,
    options: Vec<usize>,
    at: usize,
}

impl Explorer {
    fn choose(&mut self, options: usize) -> usize {
        if self.at == self.choices.len() {
            self.choices.push(0);
            self.options.push(options);
        }
        let choice = self.choices[self.at];
        assert!(
            choice < options,
            "a replayed schedule offers the same choices"
        );
        self.options[self.at] = options;
        self.at += 1;
        choice
    }

    /// Move to the next schedule; `false` once every one has been visited.
    fn advance(&mut self) -> bool {
        self.choices.truncate(self.at);
        self.options.truncate(self.at);
        while let Some(last) = self.choices.pop() {
            let options = self.options.pop().expect("aligned");
            if last + 1 < options {
                self.choices.push(last + 1);
                self.options.push(options);
                self.at = 0;
                return true;
            }
        }
        false
    }
}

struct World {
    input: Input,
    explorer: Explorer,
}

impl World {
    /// A point between two of the loop's actions: the input thread performs
    /// as many of its remaining actions as the schedule says.
    fn point(&mut self) {
        let remaining = self.input.remaining();
        let acts = self.explorer.choose(remaining + 1);
        for _ in 0..acts {
            self.input.act();
        }
    }
}

/// What one schedule did.
struct Outcome {
    /// The notifications applied, in order.
    applied: Vec<String>,
    /// For each answer the request got: the notifications applied before
    /// the run that produced it began, and those counted before its last
    /// checkpoint.
    answers: Vec<(Vec<String>, Vec<String>)>,
    runs: usize,
}

fn run_schedule(world: &RefCell<World>, inbox: &Inbox) -> Outcome {
    let mut applied = Vec::new();
    let mut waiting: VecDeque<Message> = VecDeque::new();
    let mut answers = Vec::new();
    let mut runs = 0;
    loop {
        let (watch, messages) =
            inbox.begin_step_interleaved(usize::MAX, || world.borrow_mut().point());
        waiting.extend(messages);
        if let Some(message) = waiting.pop_front() {
            let Message::Notification(notification) = message else {
                unreachable!("only notifications are sent")
            };
            applied.push(notification.method);
            world.borrow_mut().point();
            continue;
        }
        runs += 1;
        let before = applied.clone();
        let mut superseded = false;
        for _ in 0..=CHECKPOINTS {
            world.borrow_mut().point();
            if watch.check().is_err() {
                superseded = true;
                break;
            }
        }
        if superseded {
            continue;
        }
        let world = world.borrow();
        let counted: Vec<String> = world
            .input
            .counted
            .iter()
            .enumerate()
            .filter(|(_, counted)| **counted)
            .map(|(i, _)| format!("n{i}"))
            .collect();
        answers.push((before, counted));
        break;
    }
    // The input thread finishes; the loop applies what is left.
    {
        let mut world = world.borrow_mut();
        while world.input.remaining() > 0 {
            world.input.act();
        }
    }
    while let Ok(Message::Notification(notification)) = inbox.try_recv() {
        waiting.push_back(Message::Notification(notification));
    }
    for message in waiting {
        let Message::Notification(notification) = message else {
            unreachable!()
        };
        applied.push(notification.method);
    }
    Outcome {
        applied,
        answers,
        runs,
    }
}

/// Every schedule for `notifications` notifications against one request: the
/// request is answered once, with a result computed after every notification
/// counted before its last checkpoint, and every notification is applied
/// once, in order. Returns how many schedules there were, and how many of
/// them stopped a run.
fn explore(notifications: usize) -> (usize, usize) {
    let mut explorer = Explorer {
        choices: Vec::new(),
        options: Vec::new(),
        at: 0,
    };
    let mut schedules = 0;
    let mut stopped = 0;
    loop {
        let (sender, inbox) = channel(16);
        let world = RefCell::new(World {
            input: Input::new(sender, notifications),
            explorer,
        });
        let outcome = run_schedule(&world, &inbox);
        let mut world = world.into_inner();
        schedules += 1;
        let expected: Vec<String> = (0..notifications).map(|i| format!("n{i}")).collect();
        assert_eq!(
            outcome.applied, expected,
            "every notification is applied once, in order (schedule {:?})",
            world.explorer.choices
        );
        assert_eq!(
            outcome.answers.len(),
            1,
            "the request is answered once (schedule {:?})",
            world.explorer.choices
        );
        let (before, counted) = &outcome.answers[0];
        for notification in counted {
            assert!(
                before.contains(notification),
                "the answer was computed without {notification}, counted before its last \
                 checkpoint (applied before the run: {before:?}; schedule {:?})",
                world.explorer.choices
            );
        }
        if outcome.runs > 1 {
            stopped += 1;
        }
        if !world.explorer.advance() {
            break;
        }
        explorer = std::mem::replace(
            &mut world.explorer,
            Explorer {
                choices: Vec::new(),
                options: Vec::new(),
                at: 0,
            },
        );
    }
    (schedules, stopped)
}

/// The protocol's property over every interleaving of up to three
/// notifications with one request: see [`explore`]. Its non-vacuity: some
/// schedules stop the run and some do not.
#[test]
fn every_interleaving_answers_after_every_counted_notification() {
    for notifications in 0..=3 {
        let (schedules, stopped) = explore(notifications);
        assert!(schedules >= 1);
        if notifications > 0 {
            assert!(
                stopped > 0 && stopped < schedules,
                "{notifications} notifications: {stopped} of {schedules} schedules stopped a run"
            );
        }
    }
}

/// A watch that nothing can stop holds through any number of notifications.
#[test]
fn an_unwatched_computation_is_never_stopped() {
    let (sender, inbox) = channel(4);
    let watch = Watch::unwatched();
    sender
        .send(Message::Notification(Notification {
            method: "n".to_string(),
            params: serde_json::Value::Null,
        }))
        .unwrap();
    assert_eq!(watch.check(), Ok(()));
    drop(inbox);
}

/// The channel is bounded: a sender ahead of the loop by `capacity` messages
/// waits until the loop takes one, and the counter counts a notification
/// only once the channel took it.
#[test]
fn a_full_inbox_holds_the_sender_back() {
    let (sender, inbox) = channel(2);
    let notification = || {
        Message::Notification(Notification {
            method: "n".to_string(),
            params: serde_json::Value::Null,
        })
    };
    sender.send(notification()).unwrap();
    sender.send(notification()).unwrap();
    let sent = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let thread = {
        let sender = sender.clone();
        let sent = std::sync::Arc::clone(&sent);
        std::thread::spawn(move || {
            sender.send(notification()).unwrap();
            sent.store(true, Ordering::SeqCst);
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert!(!sent.load(Ordering::SeqCst), "a full inbox takes no more");
    // Taking one makes room for the third, which can only be pushed, and so
    // counted, after the counter was read.
    let (watch, messages) = inbox.begin_step(1);
    assert_eq!(messages.len(), 1);
    thread.join().unwrap();
    assert!(sent.load(Ordering::SeqCst));
    assert_eq!(
        watch.check(),
        Err(Superseded),
        "the third was counted once taken"
    );
    let (watch, messages) = inbox.begin_step(usize::MAX);
    assert_eq!(messages.len(), 2);
    assert_eq!(watch.check(), Ok(()));
}
