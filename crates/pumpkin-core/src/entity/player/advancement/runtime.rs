//! Firing criterion triggers of runtime advancements.

use std::sync::{Arc, PoisonError};

use crate::data::runtime_advancement;
use crate::entity::player::Player;

use super::PlayerAdvancement;

impl Player {
    /// Fires a trigger for this player and awards every criterion that uses it. Criteria with a
    /// `plugin_tag` condition only match when `matcher` is equal to the tag. No other condition is
    /// evaluated, the caller decides when the trigger fires. Returns how many criteria were newly
    /// awarded. Completion events, rewards, announcements and toasts work as for any award.
    pub fn trigger_criterion(self: &Arc<Self>, trigger: &str, matcher: Option<&str>) -> u32 {
        let mut awarded = 0;
        for (advancement, criterion) in runtime_advancement::criteria_for_trigger(trigger, matcher)
        {
            let result = {
                let mut guard = self
                    .advancements
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                guard.award(advancement, &criterion)
            };
            PlayerAdvancement::finish_award(self, advancement, result);
            if result.awarded() {
                awarded += 1;
            }
        }
        if awarded > 0 {
            self.advancements
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .flush_dirty(self, true);
        }
        awarded
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::advancement_data::AdvancementManager;
    use uuid::Uuid;

    const JSON: &str = r#"{
        "criteria": { "a": { "trigger": "rt_persist:fire" } },
        "display": { "icon": { "id": "minecraft:stone" }, "title": "T", "description": "D" },
        "requirements": [["a"]]
    }"#;

    #[tokio::test]
    async fn runtime_progress_persists() {
        let dir = tempfile::tempdir().unwrap();
        let manager = Arc::new(AdvancementManager::new(dir.path(), true));
        let id = Uuid::new_v4();

        runtime_advancement::add_plugin_advancement("rt_persist:root", JSON).unwrap();
        let advancement = runtime_advancement::find("rt_persist:root").unwrap();
        let mut saved = PlayerAdvancement::new(manager.clone(), id);
        let result = saved.award(advancement, "a");
        assert!(result.awarded());
        saved.save().await.unwrap();

        // Loaded before the plugin registered it again: kept, saved again, restored later.
        let _ = runtime_advancement::remove_plugin_advancement("rt_persist:root");
        let mut loaded = PlayerAdvancement::new(manager.clone(), id);
        loaded.load().unwrap();
        assert!(loaded.progress.is_empty());
        assert!(loaded.unresolved.contains_key("rt_persist:root"));
        loaded.save().await.unwrap();

        runtime_advancement::add_plugin_advancement("rt_persist:root", JSON).unwrap();
        let mut again = PlayerAdvancement::new(manager, id);
        again.load().unwrap();
        let advancement = runtime_advancement::find("rt_persist:root").unwrap();
        assert!(
            again
                .progress
                .get_mut_or_start_progress(advancement)
                .is_done()
        );
        let _ = runtime_advancement::remove_plugin_advancement("rt_persist:root");
    }
}
