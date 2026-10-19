//! Emptying one account's Trash, or every account named by the unified Trash.

use super::*;

impl MainWindow {
    pub(super) fn install_empty_trash(self: &Rc<Self>) {
        let action = gio::SimpleAction::new(
            "empty-trash",
            Some(&glib::VariantType::new("mx").expect("valid type")),
        );
        let weak = Rc::downgrade(self);
        action.connect_activate(move |_, parameter| {
            if let (Some(win), Some(account)) = (
                weak.upgrade(),
                parameter.and_then(|p| p.get::<Option<AccountId>>()),
            ) {
                win.empty_trash(account);
            }
        });
        self.actions.add_action(&action);
    }

    fn empty_trash(self: &Rc<Self>, account_id: Option<AccountId>) {
        if self.emptying_trash.replace(true) {
            return;
        }
        // Capture the accounts before asking; a later selection changes no scope.
        let accounts: Vec<_> = self
            .mail_accounts()
            .into_iter()
            .filter(|account| account_id.is_none_or(|id| id == account.id))
            .collect();
        if accounts.is_empty() {
            self.emptying_trash.set(false);
            return;
        }
        let names = accounts
            .iter()
            .map(|account| account.email.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let question = confirm(
            &gettext("Empty Trash?"),
            &fill(
                &gettext(
                    "Permanently delete every message in Trash for these accounts, including older mail not stored on this computer? This cannot be undone.\n\n{accounts}",
                ),
                &[("accounts", &names)],
            ),
            &gettext("Empty Trash"),
            Tone::Destructive,
        );
        let this = Rc::clone(self);
        glib::spawn_future_local(async move {
            if question.ask(&this.window).await {
                this.toast(&gettext("Emptying Trash…"));
                for account in accounts {
                    let actions = this.core.actions();
                    let id = account.id;
                    let result = this
                        .core
                        .call(async move { actions.empty_trash(id).await })
                        .await;
                    match result {
                        Ok(Permitted::Done(outcome)) => {
                            this.after_mail(Cause::Erased, &outcome, Some(&this.conversation));
                            match outcome.first_error() {
                                Some(error) => this.toast(&fill(
                                    &gettext("Could not empty Trash for {account}: {reason}"),
                                    &[("account", &account.email), ("reason", error)],
                                )),
                                None => this.toast(&fill_plural(
                                    "Deleted {count} message from Trash for {account}",
                                    "Deleted {count} messages from Trash for {account}",
                                    outcome.done.len(),
                                    &[
                                        ("account", &account.email),
                                        ("count", &outcome.done.len().to_string()),
                                    ],
                                )),
                            }
                        }
                        Ok(Permitted::NeedsPermission) => {
                            this.ask_permission(id, Permission::Delete, Occasion::Needed);
                            break;
                        }
                        Err(error) => this.toast(&fill(
                            &gettext("Could not empty Trash for {account}: {reason}"),
                            &[("account", &account.email), ("reason", &error.to_string())],
                        )),
                    }
                }
            }
            this.emptying_trash.set(false);
        });
    }
}
