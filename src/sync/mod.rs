use anyhow::Result;

pub mod mal;

/// Possible watch statuses mirroring MAL's status field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchStatus {
    Watching,
    Completed,
    OnHold,
    Dropped,
    PlanToWatch,
}

impl WatchStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            WatchStatus::Watching => "watching",
            WatchStatus::Completed => "completed",
            WatchStatus::OnHold => "on_hold",
            WatchStatus::Dropped => "dropped",
            WatchStatus::PlanToWatch => "plan_to_watch",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WatchStatus::Watching => "Watching",
            WatchStatus::Completed => "Completed",
            WatchStatus::OnHold => "On Hold",
            WatchStatus::Dropped => "Dropped",
            WatchStatus::PlanToWatch => "Plan to Watch",
        }
    }
}

/// Data passed to a sync provider when updating status.
#[derive(Debug, Clone)]
pub struct SyncUpdate {
    /// Anime title as displayed in anv (from AllAnime).
    pub title: String,
    /// Current episode just watched (1-indexed integer).
    pub episode: u32,
    /// Total number of episodes if known (used to decide Completed vs Watching).
    pub total_episodes: Option<u32>,
    pub status: WatchStatus,
    /// YYYY-MM-DD: set when first adding the anime to the list (not-on-list → watching).
    pub start_date: Option<String>,
    /// YYYY-MM-DD: set when the anime is marked completed.
    pub finish_date: Option<String>,
}

/// Common interface for list-sync providers (MAL, AniList, etc.).
pub trait SyncProvider: Send + Sync {
    fn name(&self) -> &'static str;
    /// Update the watch status.  The caller is responsible for
    /// user-confirmation *before* calling this.
    fn update_status(
        &self,
        update: &SyncUpdate,
    ) -> impl std::future::Future<Output = Result<()>> + Send;
}
