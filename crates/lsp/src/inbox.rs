//! The dispatch loop's input, and the one fact the thread that reads it shares
//! with the loop: how many notifications it has handed over
//! (`docs/incremental-lsp-plan.md`, Stage 9).
//!
//! The loop ([`crate::server::run_with_fetcher`]) owns every piece of the
//! server's state, and only it reads or writes any of it. What it does not
//! own is the transport. A thread of its own ([`forward`]) takes each message
//! from the transport the moment it is decoded and hands it over through an
//! [`InboxSender`]: into a bounded channel, then, for a notification, one
//! more on a shared counter. The channel and the counter are the only state
//! two threads share.
//!
//! A request that runs watches the counter ([`Watch`]): the loop reads it
//! before it reads the waiting messages for the step that runs the request
//! ([`Inbox::begin_step`]), and at each checkpoint the request compares it
//! with the value read. A notification counted since is one the loop had not
//! planned with when the run began, so the run stops there
//! ([`Superseded`]) and the loop plans again with it.
//!
//! **Why the order matters.** The sender counts a notification only after it
//! is in the channel, and the loop reads the counter before it reads the
//! channel. So a notification counted before the loop's read is in the
//! channel when the loop reads it, and is planned before the run begins; a
//! notification counted after the read moves the counter the run watches. A
//! sender that counted first could let the loop read a count that includes a
//! notification still on its way into the channel, and run a request that
//! never sees it with no checkpoint to notice. A loop that read the channel
//! before the counter could miss a notification that landed between the two
//! reads. `tests::every_interleaving_answers_after_every_counted_notification`
//! holds both orders to that, over every interleaving.
//!
//! The counter counts what was handed over, not what changes a text: a
//! cancellation of another request, or a notification the server ignores,
//! stops a running request too, which then runs again. That costs at most
//! the rest of the unit the request was in, and keeps the sender free of
//! any knowledge of what the loop is doing.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_channel::{Receiver, RecvError, SendError, Sender, TryRecvError};
use lsp_server::Message;

/// The name of the `tracing` event [`Watch::check`] emits at every checkpoint,
/// before it reads the counter. Tests send a notification from a subscriber
/// on it, which makes the running request stop at that very checkpoint.
pub const CHECKPOINT_EVENT: &str = "inbox.checkpoint";

/// The name of the `tracing` event the loop emits when a request's run stops
/// at a checkpoint, with the request's `method`.
pub const STOPPED_EVENT: &str = "inbox.stopped";

/// A running request stopped at a checkpoint: a notification was handed to
/// the loop after the loop read the counter for the request's run. Whatever
/// the run computed and cached stands; its answer is not sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Superseded;

/// The loop's side of the input: the channel the messages arrive on, and the
/// count of notifications handed over.
pub struct Inbox {
    messages: Receiver<Message>,
    notifications: Arc<AtomicU64>,
}

/// The input thread's side: hands a message to the loop, then counts it if it
/// is a notification.
#[derive(Clone)]
pub struct InboxSender {
    messages: Sender<Message>,
    notifications: Arc<AtomicU64>,
}

/// A connected pair whose channel holds at most `capacity` messages; a
/// sender ahead of that waits.
pub fn channel(capacity: usize) -> (InboxSender, Inbox) {
    let (sender, receiver) = crossbeam_channel::bounded(capacity);
    let notifications = Arc::new(AtomicU64::new(0));
    (
        InboxSender {
            messages: sender,
            notifications: Arc::clone(&notifications),
        },
        Inbox {
            messages: receiver,
            notifications,
        },
    )
}

/// The loop's input from `transport`: a thread of its own takes each message
/// from it as soon as it arrives and hands it over through a channel of
/// `capacity` ([`InboxSender::send`]). The thread ends when the transport
/// ends or the loop drops the [`Inbox`].
///
/// `lsp-server`'s stdio transport hands each message over a rendezvous
/// channel and decodes the next only once the last was taken. Read directly,
/// a burst the client has sent would sit in stdin while the loop handled its
/// first message, and the loop would find nothing waiting to plan with it
/// ([`crate::batch`]); and a notification sent while a request runs would not
/// be counted until the request finished, so the request could not stop for
/// it.
pub fn forward(transport: Receiver<Message>, capacity: usize) -> Inbox {
    let (sender, inbox) = channel(capacity);
    std::thread::Builder::new()
        .name("borzoi-input".to_string())
        .spawn(move || {
            for message in transport {
                if sender.send(message).is_err() {
                    break;
                }
            }
        })
        .expect("spawn the input thread");
    inbox
}

/// One action of handing a message over ([`handover`]).
#[derive(Debug)]
enum Handover {
    /// Into the channel, waiting while it is full.
    Push(Message),
    /// One more on the counter. `Release`, so a loop that reads the new count
    /// also sees the message in the channel.
    Count,
}

/// The actions that hand `message` over, in order: into the channel, then,
/// for a notification, counted. A description, so that the interleaving test
/// schedules the very actions [`InboxSender::send`] performs.
fn handover(message: Message) -> Vec<Handover> {
    let notification = matches!(message, Message::Notification(_));
    let mut actions = vec![Handover::Push(message)];
    if notification {
        actions.push(Handover::Count);
    }
    actions
}

impl InboxSender {
    /// Hand `message` to the loop: into the channel, waiting while it is
    /// full, and then, for a notification, counted. Errs only when the loop
    /// has dropped its [`Inbox`].
    pub fn send(&self, message: Message) -> Result<(), SendError<Message>> {
        for action in handover(message) {
            self.perform(action)?;
        }
        Ok(())
    }

    fn perform(&self, action: Handover) -> Result<(), SendError<Message>> {
        match action {
            Handover::Push(message) => self.messages.send(message),
            Handover::Count => {
                self.notifications.fetch_add(1, Ordering::Release);
                Ok(())
            }
        }
    }
}

impl Inbox {
    /// Read the counter, then every message waiting, up to `room` of them:
    /// the start of a step. The [`Watch`] holds the count read, so a request
    /// the step runs stops for any notification not among `messages` or
    /// planned before them.
    pub fn begin_step(&self, room: usize) -> (Watch, Vec<Message>) {
        self.begin_step_interleaved(room, || {})
    }

    /// [`Self::begin_step`], calling `between` after reading the counter and
    /// after each attempt to take a message: the points where the input
    /// thread can act while the loop begins a step. The exhaustive
    /// interleaving test acts there; the loop passes nothing.
    fn begin_step_interleaved(
        &self,
        room: usize,
        mut between: impl FnMut(),
    ) -> (Watch, Vec<Message>) {
        let watch = Watch {
            notifications: Some(Arc::clone(&self.notifications)),
            seen: self.notifications.load(Ordering::Acquire),
        };
        between();
        let mut messages = Vec::new();
        while messages.len() < room {
            let taken = self.messages.try_recv();
            between();
            match taken {
                Ok(message) => messages.push(message),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        (watch, messages)
    }

    /// Wait for the next message. Errs when the input has ended.
    pub fn recv(&self) -> Result<Message, RecvError> {
        self.messages.recv()
    }

    /// The next message if one is waiting.
    pub fn try_recv(&self) -> Result<Message, TryRecvError> {
        self.messages.try_recv()
    }
}

/// What a running request compares at its checkpoints: the count of
/// notifications the loop read before the messages its run was planned with.
#[derive(Clone, Debug)]
pub struct Watch {
    /// `None` for a computation no notification can stop ([`Self::unwatched`]).
    notifications: Option<Arc<AtomicU64>>,
    seen: u64,
}

impl Watch {
    /// A watch that never sees a notification: for a computation outside a
    /// request the loop runs (a unit test, the workspace sweep, a caller
    /// that wants the answer whatever arrives).
    pub fn unwatched() -> Watch {
        Watch {
            notifications: None,
            seen: 0,
        }
    }

    /// A checkpoint: `Err` once a notification has been handed over since the
    /// count this watch holds was read. Emits [`CHECKPOINT_EVENT`] first.
    pub fn check(&self) -> Result<(), Superseded> {
        tracing::trace!(name: CHECKPOINT_EVENT, "checkpoint");
        match &self.notifications {
            Some(counter) if counter.load(Ordering::Acquire) != self.seen => Err(Superseded),
            _ => Ok(()),
        }
    }
}

/// The value of a computation run under [`Watch::unwatched`], which no
/// checkpoint can stop.
pub fn never_superseded<T>(result: Result<T, Superseded>) -> T {
    result.unwrap_or_else(|Superseded| {
        unreachable!("an unwatched computation stopped at a checkpoint")
    })
}

#[cfg(test)]
mod tests;
