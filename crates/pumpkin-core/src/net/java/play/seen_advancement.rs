#[allow(clippy::wildcard_imports)]
use super::*;

impl JavaClient {
    pub fn handle_seen_advancement(&self, player: &Arc<Player>, packet: &SSeenAdvancement) {
        if let SSeenAdvancement::OpenTab(tab) = packet {
            let advancement = crate::data::runtime_advancement::find(&tab.to_string());
            if advancement.is_some() {
                player
                    .advancements
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .set_selected_tab(advancement);
            }
        }
    }
}
