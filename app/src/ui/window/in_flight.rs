//! Mail actions on their way to the server, as the window shows them.
//!
//! An action changes the store and then waits for the server, which can
//! take a while behind a sync pass or a slow connection. The window does
//! not wait with it. The rows the action takes out of the list leave at
//! once and stay out of every refresh until the action ends, and its
//! toast shows at once with an Undo that reverses that action and no
//! later one. Undo pressed while the action still runs, on the toast or
//! with the keyboard, waits for it. Quick actions of one kind share a
//! toast, "Moved 3 conversations to Trash", whose Undo takes all of them
//! back.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::glib;
use mailrs_domain::translate::{fill, gettext};
use mailrs_domain::{Target, ThreadSummary};
use mailrs_sync::{MailAction, Outcome, Recorded, Undone};

use super::MainWindow;
use super::aftermath::covers;

/// How long an action's toast stays, in seconds.
const TOAST_SECONDS: u32 = 5;

/// One recorded action, from the moment it starts.
pub(super) struct Run {
    state: Cell<State>,
    /// Undo was pressed before the action ended.
    undo_wanted: Cell<bool>,
    /// The conversations or messages it was asked to change, then the
    /// ones it changed once it ends.
    count: Cell<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Running,
    /// On the undo stack, under this record.
    Recorded(Recorded),
    /// Failed, recorded nothing, or undone already.
    Gone,
}

/// What a press of Undo does to one action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pressed {
    /// Reverse this record now.
    Now(Recorded),
    /// The action still runs; it is reversed when it ends.
    Later,
    Nothing,
}

impl Run {
    fn new(count: usize) -> Run {
        Run {
            state: Cell::new(State::Running),
            undo_wanted: Cell::new(false),
            count: Cell::new(count),
        }
    }

    fn running(&self) -> bool {
        self.state.get() == State::Running
    }

    /// The action ended. Answers the record to undo now when Undo was
    /// pressed while it ran.
    fn finish(&self, recorded: Option<Recorded>) -> Option<Recorded> {
        if self.undo_wanted.get() {
            self.state.set(State::Gone);
            return recorded;
        }
        self.state
            .set(recorded.map_or(State::Gone, State::Recorded));
        None
    }

    fn undo(&self) -> Pressed {
        match self.state.get() {
            State::Running => {
                self.undo_wanted.set(true);
                Pressed::Later
            }
            State::Recorded(recorded) => {
                self.state.set(State::Gone);
                Pressed::Now(recorded)
            }
            State::Gone => Pressed::Nothing,
        }
    }
}

/// The toast on screen for one or more actions of one kind.
struct Shown {
    toast: adw::Toast,
    action: MailAction,
    /// The toast's own words, which stand alone; `None` when it is worded
    /// by count.
    message: Option<String>,
    runs: Rc<RefCell<Vec<Rc<Run>>>>,
}

impl Shown {
    fn count(&self) -> usize {
        self.runs.borrow().iter().map(|run| run.count.get()).sum()
    }

    fn holds(&self, run: &Rc<Run>) -> bool {
        self.runs.borrow().iter().any(|held| Rc::ptr_eq(held, run))
    }
}

/// A running action's rows, held out of the list.
struct Held {
    id: u64,
    targets: Vec<Target>,
}

#[derive(Default)]
pub(super) struct InFlight {
    next: Cell<u64>,
    leaving: RefCell<Vec<Held>>,
    shown: RefCell<Option<Shown>>,
    /// The newest recorded action, which the keyboard's Undo reaches
    /// while it still runs.
    newest: RefCell<Option<Rc<Run>>>,
}

/// One action's rows in [`InFlight`]'s list of held rows.
#[derive(Debug, Clone, Copy)]
pub(super) struct Leaving(u64);

impl InFlight {
    /// Holds `targets` out of the list until [`Self::arrived`].
    pub(super) fn leave(&self, targets: &[Target]) -> Leaving {
        let id = self.next.get() + 1;
        self.next.set(id);
        self.leaving.borrow_mut().push(Held {
            id,
            targets: targets.to_vec(),
        });
        Leaving(id)
    }

    /// The action ended: its rows list again where the store has them.
    pub(super) fn arrived(&self, leaving: Leaving) {
        self.leaving
            .borrow_mut()
            .retain(|held| held.id != leaving.0);
    }

    /// Drops the rows running actions took out.
    pub(super) fn keep_out(&self, rows: Vec<ThreadSummary>) -> Vec<ThreadSummary> {
        let leaving = self.leaving.borrow();
        if leaving.is_empty() {
            return rows;
        }
        rows.into_iter()
            .filter(|row| {
                !leaving
                    .iter()
                    .any(|held| held.targets.iter().any(|target| covers(target, row)))
            })
            .collect()
    }

    /// The newest action, when it still runs.
    fn newest_running(&self) -> Option<Rc<Run>> {
        self.newest.borrow().clone().filter(|run| run.running())
    }

    /// The actions the toast on screen speaks for, with `run` added, when
    /// `run` joins it: the same action, worded by count.
    fn join(
        &self,
        action: &MailAction,
        worded_by_count: bool,
        run: &Rc<Run>,
    ) -> Option<Rc<RefCell<Vec<Rc<Run>>>>> {
        let shown = self.shown.borrow();
        let on_screen = shown.as_ref()?;
        if !(worded_by_count && on_screen.message.is_none() && on_screen.action == *action) {
            return None;
        }
        on_screen.runs.borrow_mut().push(Rc::clone(run));
        Some(Rc::clone(&on_screen.runs))
    }

    /// Takes the toast on screen down, if any. The borrow ends first:
    /// dismissing emits `dismissed`, whose handler borrows `shown` again.
    fn take_down(&self) {
        let gone = self.shown.borrow_mut().take();
        if let Some(gone) = gone {
            gone.toast.dismiss();
        }
    }
}

impl MainWindow {
    /// Starts showing a recorded action: its toast at once, or the toast on
    /// screen counting it too, which then starts its time again. `message`
    /// words the toast in place of the count, and such a toast stands
    /// alone. An action with no words gets no toast, and the keyboard's
    /// Undo still reaches it.
    pub(super) fn announce(
        self: &Rc<Self>,
        action: &MailAction,
        count: usize,
        message: Option<String>,
    ) -> Rc<Run> {
        let run = Rc::new(Run::new(count));
        self.in_flight.newest.replace(Some(Rc::clone(&run)));
        let runs = self
            .in_flight
            .join(action, message.is_none(), &run)
            .unwrap_or_else(|| Rc::new(RefCell::new(vec![Rc::clone(&run)])));
        self.show_toast(action.clone(), message, runs);
        run
    }

    /// Puts up the toast for `runs` in place of the one on screen.
    fn show_toast(
        self: &Rc<Self>,
        action: MailAction,
        message: Option<String>,
        runs: Rc<RefCell<Vec<Rc<Run>>>>,
    ) {
        self.in_flight.take_down();
        let count = runs.borrow().iter().map(|run| run.count.get()).sum();
        let threading = self.settings_with(|s| s.threading);
        let Some(title) = message
            .clone()
            .or_else(|| super::done_message(&action, count, threading))
        else {
            return;
        };
        // High, so the newest action's toast shows now instead of queueing
        // behind the last one.
        let toast = adw::Toast::builder()
            .title(super::toast_title(&title))
            .button_label(gettext("Undo"))
            .timeout(TOAST_SECONDS)
            .priority(adw::ToastPriority::High)
            .build();
        let (weak, undoing) = (Rc::downgrade(self), Rc::clone(&runs));
        toast.connect_button_clicked(move |_| {
            if let Some(win) = weak.upgrade() {
                win.undo_runs(&undoing.borrow());
            }
        });
        let weak = Rc::downgrade(self);
        toast.connect_dismissed(move |gone| {
            let Some(win) = weak.upgrade() else { return };
            let mut shown = win.in_flight.shown.borrow_mut();
            if shown.as_ref().is_some_and(|s| &s.toast == gone) {
                shown.take();
            }
        });
        self.toasts.add_toast(toast.clone());
        self.in_flight.shown.replace(Some(Shown {
            toast,
            action,
            message,
            runs,
        }));
    }

    /// The action behind `run` ended. Its toast counts what it changed,
    /// or goes when the actions it speaks for changed nothing, and the
    /// action is undone now if Undo was pressed while it ran.
    pub(super) fn ran(self: &Rc<Self>, run: &Rc<Run>, outcome: &Outcome) {
        let undo_now = run.finish(outcome.recorded);
        let was = run.count.replace(outcome.done.len());
        let retitle = {
            let shown = self.in_flight.shown.borrow();
            shown
                .as_ref()
                .filter(|s| s.holds(run))
                .map(|s| (s.count(), s.action.clone(), s.message.is_none()))
        };
        match retitle {
            Some((0, ..)) => self.in_flight.take_down(),
            Some((count, action, true)) if count != was => {
                let threading = self.settings_with(|s| s.threading);
                if let (Some(title), Some(shown)) = (
                    super::done_message(&action, count, threading),
                    self.in_flight.shown.borrow().as_ref(),
                ) {
                    shown.toast.set_title(&super::toast_title(&title));
                }
            }
            _ => {}
        }
        if let Some(recorded) = undo_now {
            self.undo_recorded(vec![recorded]);
        }
    }

    /// The keyboard's Undo: the newest action while it still runs, which
    /// waits for it to end, or the top of the undo stack.
    pub(super) fn undo_newest(self: &Rc<Self>) -> bool {
        let Some(run) = self.in_flight.newest_running() else {
            return false;
        };
        run.undo();
        self.say_now(&gettext("Undo will run when this finishes"));
        true
    }

    /// The toast's Undo: each action it speaks for, newest first. One
    /// still running is undone when it ends.
    fn undo_runs(self: &Rc<Self>, runs: &[Rc<Run>]) {
        let pressed: Vec<Pressed> = runs.iter().rev().map(|run| run.undo()).collect();
        let now: Vec<Recorded> = pressed
            .iter()
            .filter_map(|p| match p {
                Pressed::Now(recorded) => Some(*recorded),
                _ => None,
            })
            .collect();
        if !now.is_empty() {
            self.undo_recorded(now);
        } else if pressed.contains(&Pressed::Later) {
            self.say_now(&gettext("Undo will run when this finishes"));
        }
    }

    /// Reverses these actions in order and says how it went: the first
    /// failure, or what the last one took back.
    fn undo_recorded(self: &Rc<Self>, records: Vec<Recorded>) {
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            let mut said: Option<Undone> = None;
            for recorded in records {
                let Some(undone) = this.core.undo_this(recorded).await else {
                    continue;
                };
                this.after_mail(super::aftermath::Cause::Undid, &undone.outcome, None);
                let failed_before = said
                    .as_ref()
                    .is_some_and(|s| s.outcome.first_error().is_some());
                if !failed_before {
                    said = Some(undone);
                }
            }
            this.say_undone(said);
        });
    }

    /// The toast after an undo: what it took back, or why it could not.
    pub(super) fn say_undone(self: &Rc<Self>, undone: Option<Undone>) {
        let Some(undone) = undone else {
            return self.say_now(&gettext("Nothing to undo"));
        };
        match undone.outcome.first_error() {
            Some(error) => self.say_now(error),
            None => self.say_now(&fill(
                &gettext("{action} undone"),
                &[("action", &undone.words)],
            )),
        }
    }

    /// A toast that shows at once, beside the action toasts that do.
    pub(super) fn say_now(&self, text: &str) {
        self.toasts.add_toast(
            adw::Toast::builder()
                .title(super::toast_title(text))
                .timeout(4)
                .priority(adw::ToastPriority::High)
                .build(),
        );
    }
}

#[cfg(test)]
mod tests {
    use mailrs_domain::Target;

    use super::*;

    #[test]
    fn undo_pressed_while_the_action_runs_waits_for_it() {
        let run = Run::new(1);
        assert_eq!(run.undo(), Pressed::Later, "nothing to undo yet");
        let recorded = record();
        assert_eq!(
            run.finish(Some(recorded)),
            Some(recorded),
            "undone once it ends"
        );
        assert_eq!(run.undo(), Pressed::Nothing, "and only once");
    }

    #[test]
    fn undo_after_the_action_ended_takes_its_own_record() {
        let run = Run::new(1);
        let recorded = record();
        assert_eq!(run.finish(Some(recorded)), None);
        assert_eq!(run.undo(), Pressed::Now(recorded));
        assert_eq!(run.undo(), Pressed::Nothing);
    }

    #[test]
    fn a_failed_action_leaves_nothing_to_undo() {
        let run = Run::new(1);
        assert_eq!(run.finish(None), None);
        assert_eq!(run.undo(), Pressed::Nothing);
    }

    #[test]
    fn the_keyboard_reaches_the_newest_action_only_while_it_runs() {
        let in_flight = InFlight::default();
        let run = Rc::new(Run::new(1));
        in_flight.newest.replace(Some(Rc::clone(&run)));
        assert!(in_flight.newest_running().is_some());
        run.finish(Some(record()));
        assert!(
            in_flight.newest_running().is_none(),
            "then the undo stack answers"
        );
    }

    #[test]
    fn rows_stay_out_until_their_action_arrives() {
        let in_flight = InFlight::default();
        let row = |id: &str| ThreadSummary {
            account_id: 1,
            id: id.into(),
            ..Default::default()
        };
        let leaving = in_flight.leave(&[Target::thread(1, "t1")]);
        let kept = in_flight.keep_out(vec![row("t1"), row("t2")]);
        assert_eq!(
            kept.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["t2"]
        );
        in_flight.arrived(leaving);
        assert_eq!(in_flight.keep_out(vec![row("t1")]).len(), 1);
    }

    fn record() -> Recorded {
        mailrs_sync::Recorded::for_tests(7)
    }
}
