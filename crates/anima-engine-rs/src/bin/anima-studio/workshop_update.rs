//! The messages this window handles on its own, moved out of `handle` so that it dispatches
//! rather than works. Each was an arm of that match, verbatim, with the types the enum declares.

use crate::*;

impl Studio {
    pub(crate) fn workshop_refresh(&mut self) -> Task<Message> {
        self.refresh();
        // A refresh is also the moment to pick up titles for anything new.
        if let Some(task) = installed_meta_task(self) {
            return task;
        }
        Task::none()
    }

    pub(crate) fn workshop_search(&mut self, query: String) -> Task<Message> {
        // Typing only filters. Every keystroke used to open the picker, which popped the
        // installed list up under the cursor while the user was still typing a word.
        // Enter, or the button next to the field, is what opens it.
        self.query = query;
        Task::none()
    }

    pub(crate) fn workshop_clear_search(&mut self) -> Task<Message> {
        self.query.clear();
        // The box and the list have to agree: with the box empty, Steam results for the
        // query that was just deleted have no business being on screen.
        if self.remote.is_some() {
            self.leave_remote();
        }
        Task::none()
    }

    pub(crate) fn workshop_search_steam(&mut self) -> Task<Message> {
        // A new search starts at the first page, with no cursor history behind it.
        self.remote_query = search_text(&self.query);
        self.remote_page = 1;
        self.remote_cursors = vec![None];
        self.remote_next = None;
        self.remote_total = 0;
        self.goto_remote_page(1)
    }

    pub(crate) fn workshop_remote_results(
        &mut self,
        request: u64,
        page: u32,
        loaded: RemotePage,
    ) -> Task<Message> {
        if request != self.remote_request {
            // A reply to a search the user has already left.
            crate::log::event(
                crate::log::Level::Debug,
                "studio.workshop.stale_reply",
                &[("page", &page)],
            );
            return Task::none();
        }
        self.remote_page = page;
        self.remote_total = loaded.total;
        self.remote_next = loaded.next_cursor;
        self.remote_loading = false;
        // Steam counts its matches, so the pager can say how many pages there are instead
        // of making the user walk to the end to find out.
        self.remote_note = match loaded.rows.len() {
            0 => self.i18n.tr("no Workshop results"),
            _ if loaded.total > 0 => fill(
                &self.i18n.tr("about {} Workshop result(s) · page {}/{}"),
                &[
                    loaded.total.to_string(),
                    page.to_string(),
                    page_count(loaded.total, REMOTE_PER_PAGE).to_string(),
                ],
            ),
            count => format!("{count} Workshop result(s)"),
        };
        self.remote = Some(loaded.rows);
        Task::none()
    }

    pub(crate) fn workshop_remote_results_2(
        &mut self,
        request: u64,
        error: String,
    ) -> Task<Message> {
        if request != self.remote_request {
            return Task::none();
        }
        self.remote_loading = false;
        self.remote = None;
        self.remote_note = error;
        Task::none()
    }

    pub(crate) fn workshop_copy_path(&mut self, path: String) -> Task<Message> {
        self.note = format!("{}: {path}", self.i18n.tr("copied"));
        iced::clipboard::write(path)
    }

    pub(crate) fn workshop_open_page(&mut self, id: u64) -> Task<Message> {
        // Subscribe and unsubscribe both live in the Steam client; this is the
        // page that carries the toggle.
        let url = format!("steam://url/CommunityFilePage/{id}");
        self.note = match std::process::Command::new("xdg-open").arg(&url).spawn() {
            Ok(_) => format!("opened {url} — subscribe/unsubscribe there"),
            Err(error) => format!("{} {url}: {error}", self.i18n.tr("could not open")),
        };
        Task::none()
    }

    pub(crate) fn workshop_arm_remove(&mut self, id: u64) -> Task<Message> {
        self.pending_remove = Some(id);
        self.note = self.i18n.tr("confirm the removal in the warning");
        Task::none()
    }

    pub(crate) fn workshop_remove(&mut self, id: u64) -> Task<Message> {
        self.pending_remove = None;
        // The tuning area must not keep pointing at a file the removal takes away.
        let gone = self.previewed_asset.as_ref().is_some_and(|asset| {
            self.item(id)
                .is_some_and(|item| asset.starts_with(&item.path))
        });
        if gone {
            self.previewed_asset = None;
            self.staged = None;
        }
        let root = self.steamcmd_root.clone();
        let Some(item) = self.snapshot.items.get(&id) else {
            return Task::none();
        };
        let ours = root
            .as_ref()
            .is_some_and(|root| item.path.starts_with(root));
        if !ours {
            self.note = self
                .i18n
                .tr("only our own SteamCMD downloads can be removed here; unsubscribe in Steam");
        } else {
            let path = item.path.clone();
            self.note = match std::fs::remove_dir_all(&path) {
                Ok(()) => {
                    self.thumbs.remove(&id);
                    self.meta.remove(&id);
                    format!("{} {id}", self.i18n.tr("removed the local copy of"))
                }
                Err(error) => format!("remove failed: {error}"),
            };
            self.refresh();
        }
        Task::none()
    }

    pub(crate) fn workshop_poll_workshop(&mut self) -> Task<Message> {
        // Read every tick, unconditionally. The first version skipped the read unless something was
        // already known to be pending, which made the whole thing unable to start: the single
        // observation that could have told it "yes, there is one" was the observation it refused to
        // make. It is one small file twice a second; there was never a cost worth that.
        let roots = WorkshopWatcher::discover().roots().to_vec();
        self.refresh_states(&roots);
        Task::none()
    }

    pub(crate) fn workshop_cancel_remove(&mut self) -> Task<Message> {
        self.pending_remove = None;
        self.note = self.i18n.tr("cancelled");
        Task::none()
    }

    pub(crate) fn workshop_meta_loaded(&mut self, loaded: Vec<(u64, Meta)>) -> Task<Message> {
        let count = loaded.len();
        for (id, meta) in loaded {
            self.meta.insert(id, meta);
        }
        crate::log::event(
            crate::log::Level::Debug,
            "studio.workshop.metadata",
            &[("count", &count)],
        );
        Task::none()
    }
}
