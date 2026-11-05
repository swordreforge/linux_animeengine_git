//! The messages this window handles on its own, moved out of `handle` so that it dispatches
//! rather than works. Each was an arm of that match, verbatim, with the types the enum declares.

use crate::*;

impl Studio {
    pub(crate) fn library_select(&mut self, id: u64) -> Task<Message> {
        // A different item means a different set of assets: the one on show, and the
        // parameters staged for it, belong to the old item and go with it.
        if self.selected_item != Some(id) {
            self.previewed_asset = None;
            self.staged = None;
        }
        self.selected_item = Some(id);
        // A remote result has no directory yet; its preview comes from the CDN.
        let dir = self
            .item(id)
            .map(|item| item.path.clone())
            .unwrap_or_default();
        if !self.thumbs.contains_key(&id) {
            return self.load_thumb_task(id, dir);
        }
        Task::none()
    }

    pub(crate) fn library_loaded(
        &mut self,
        id: u64,
        meta: Option<Meta>,
        thumb: Result<(u32, u32, Vec<u8>), String>,
    ) -> Task<Message> {
        if let Some(meta) = meta {
            self.meta.insert(id, meta);
        }
        if thumb.is_ok() && self.selected_item == Some(id) {
            self.item_fade = fade_in(self.now, self.motion);
        }
        self.thumbs.insert(
            id,
            match thumb {
                Ok((width, height, rgba)) => {
                    Thumb::Ready(image::Handle::from_rgba(width, height, rgba))
                }
                Err(error) => Thumb::Failed(error),
            },
        );
        Task::none()
    }
}
