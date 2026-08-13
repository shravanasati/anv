use std::future::Future;
use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use dialoguer::{FuzzySelect, Select};

use crate::history::{History, HistoryEntry, theme};
use crate::types::{Provider, Translation};
use crate::utils::{next_episode_label_presorted, sorted_episode_labels};

/// A single selectable episode/chapter in the playback/reading loop.
#[derive(Debug, Clone)]
pub struct MediaEntry {
    /// Provider identifier (show ID / manga ID) used for history.
    pub id: String,
    /// Display label (episode number / chapter label).
    pub label: String,
}

/// Outcome of a single `consume` invocation in [`run_media_loop`].
#[derive(Debug, Clone)]
pub enum ConsumeOutcome {
    /// Abort this iteration and try again with `current` unchanged.
    Retry,
    /// Abort this iteration and continue from the given label.
    RetryWith(String),
    /// The item was consumed (player/viewer launched); record history and advance.
    Consumed,
}

/// Per-iteration context passed to the `consume` closure.
#[allow(dead_code)]
pub struct MediaContext<'a> {
    /// Highest (latest) available label, used to reposition on fetch failures.
    pub latest: &'a str,
    /// The next label after the current one, if any.
    pub next_candidate: Option<&'a str>,
    /// 1-based position of the current label in the sorted list (for MAL sync).
    pub ep_num: u32,
    /// Whether the current label was selected without user input.
    pub auto_advance: bool,
}

/// Display configuration for the shared media loop.
pub struct MediaLoopConfig<'a> {
    pub select_prompt: &'a str,
    pub use_fuzzy: bool,
    pub unit_plural: &'a str,
    pub unit_singular: &'a str,
    pub last_verb: &'a str,
}

/// Shared interactive "select and consume" loop used by both anime playback and
/// manga reading.
///
/// Handles start-label resolution, episode/chapter selection, history updates,
/// and auto-advance. The caller supplies a `consume` closure that fetches and
/// launches the media for the selected entry. The closure must clone anything it
/// needs from `entry`/`ctx` into owned values *before* an inner `async move`
/// block so the returned future does not borrow its arguments.
pub async fn run_media_loop<F, Fut>(
    title: &str,
    items: Vec<MediaEntry>,
    prefer: Option<String>,
    last_watched: Option<String>,
    auto_play_next: bool,
    binge: bool,
    history: &mut History,
    history_path: &Path,
    translation: Translation,
    provider: Provider,
    cfg: MediaLoopConfig<'_>,
    consume: F,
) -> Result<()>
where
    F: FnMut(&MediaEntry, &MediaContext) -> Fut,
    Fut: Future<Output = Result<ConsumeOutcome>>,
{
    let labels: Vec<String> = items.iter().map(|e| e.label.clone()).collect();
    let sorted = sorted_episode_labels(&labels);

    let latest = sorted
        .last()
        .cloned()
        .expect("items is non-empty; callers bail on empty lists");
    println!(
        "Found {} {} {}. Latest available: {}.",
        labels.len(),
        translation.label(),
        cfg.unit_plural,
        latest
    );

    if let Some(prev) = &last_watched {
        println!(
            "Last {} {} {}: {}.",
            cfg.last_verb,
            translation.label(),
            cfg.unit_singular,
            prev
        );
    }

    let (mut current, mut skip_selection) = resolve_start_label(
        &StartContext {
            labels: &labels,
            sorted: &sorted,
            latest: &latest,
            title,
            unit_singular: cfg.unit_singular,
        },
        prefer.as_deref(),
        last_watched.as_deref(),
        auto_play_next,
    );

    let theme = theme();
    let mut consume = consume;
    loop {
        let default_idx = labels
            .iter()
            .position(|l| l == &current)
            .or_else(|| labels.iter().position(|l| l == &latest))
            .unwrap_or(0);

        let idx = if skip_selection {
            skip_selection = false;
            default_idx
        } else {
            let selection = if cfg.use_fuzzy {
                FuzzySelect::with_theme(&theme)
                    .with_prompt(cfg.select_prompt)
                    .items(&labels)
                    .default(default_idx)
                    .interact_opt()?
            } else {
                Select::with_theme(&theme)
                    .with_prompt(cfg.select_prompt)
                    .items(&labels)
                    .default(default_idx)
                    .interact_opt()?
            };
            let Some(i) = selection else {
                println!("Exiting playback loop.");
                return Ok(());
            };
            i
        };

        let chosen = items[idx].clone();
        let auto_advance = idx == default_idx;
        let next_candidate = next_episode_label_presorted(&chosen.label, &sorted);
        let ep_num = sorted
            .iter()
            .position(|l| l == &chosen.label)
            .map(|pos| (pos + 1) as u32)
            .unwrap_or_else(|| chosen.label.parse::<u32>().unwrap_or(0));

        let outcome = consume(
            &chosen,
            &MediaContext {
                latest: &latest,
                next_candidate: next_candidate.as_deref(),
                ep_num,
                auto_advance,
            },
        )
        .await?;

        match outcome {
            ConsumeOutcome::Retry => continue,
            ConsumeOutcome::RetryWith(label) => {
                current = label;
                continue;
            }
            ConsumeOutcome::Consumed => {}
        }

        history.upsert(HistoryEntry {
            show_id: chosen.id.clone(),
            show_title: title.to_string(),
            episode: chosen.label.clone(),
            translation,
            provider,
            watched_at: Utc::now(),
        });
        history.save(history_path)?;

        match advance_current(
            auto_advance || binge,
            next_candidate,
            chosen.label,
            cfg.unit_plural,
        ) {
            Some(next) => {
                current = next;
                if binge {
                    skip_selection = true;
                }
            }
            None => return Ok(()),
        }
    }
}

/// Read-only inputs the start-label resolution reads from the media list.
struct StartContext<'a> {
    labels: &'a [String],
    sorted: &'a [String],
    latest: &'a str,
    title: &'a str,
    unit_singular: &'a str,
}

/// Resolve the starting label from the user's preference, last-watched entry,
/// and auto-advance setting. Returns `(label, skip_selection)`.
fn resolve_start_label(
    ctx: &StartContext<'_>,
    prefer: Option<&str>,
    last_watched: Option<&str>,
    auto_play_next: bool,
) -> (String, bool) {
    let fallback = last_watched.unwrap_or(ctx.latest);
    let unit_cap = {
        let mut chars = ctx.unit_singular.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        }
    };
    match prefer {
        Some(ep) if ctx.labels.iter().any(|l| l == ep) => (ep.to_string(), true),
        Some(ep) => {
            let by_index = ep
                .parse::<usize>()
                .ok()
                .filter(|&n| n >= 1)
                .and_then(|n| ctx.sorted.get(n - 1));
            if let Some(resolved) = by_index {
                println!(
                    "{} '{}' not found by label; resuming at index {} → {} '{}'.",
                    unit_cap,
                    ep,
                    ep.parse::<usize>().unwrap(),
                    ctx.unit_singular,
                    resolved
                );
                (resolved.clone(), true)
            } else {
                println!(
                    "{} '{}' does not exist for '{}'. Showing {} list.",
                    unit_cap, ep, ctx.title, ctx.unit_singular
                );
                (fallback.to_string(), false)
            }
        }
        None => {
            if auto_play_next {
                if let Some(last) = last_watched {
                    if let Some(next) = next_episode_label_presorted(last, ctx.sorted) {
                        (next, true)
                    } else {
                        (last.to_string(), false)
                    }
                } else {
                    (
                        ctx.sorted
                            .first()
                            .cloned()
                            .unwrap_or_else(|| ctx.latest.to_string()),
                        true,
                    )
                }
            } else {
                (fallback.to_string(), false)
            }
        }
    }
}

/// Compute the next label after a consumed item. Returns `None` to exit the loop.
fn advance_current(
    auto_advance: bool,
    next_candidate: Option<String>,
    chosen: String,
    unit_plural: &str,
) -> Option<String> {
    match (auto_advance, next_candidate) {
        (true, Some(next)) => Some(next),
        (true, None) => {
            println!("No further {unit_plural} found. Exiting.");
            None
        }
        (false, candidate) => Some(candidate.unwrap_or(chosen)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[test]
    fn test_advance_current() {
        assert_eq!(
            advance_current(true, Some("2".to_string()), "1".to_string(), "episodes"),
            Some("2".to_string())
        );
        assert_eq!(
            advance_current(true, None, "10".to_string(), "episodes"),
            None
        );
        assert_eq!(
            advance_current(false, Some("2".to_string()), "1".to_string(), "episodes"),
            Some("2".to_string())
        );
    }

    #[tokio::test]
    async fn test_binge_mode_auto_plays_all_episodes() {
        let history_path = std::env::temp_dir().join("anv_test_binge_history.json");
        let mut history = History::default();

        let items = vec![
            MediaEntry {
                id: "1".to_string(),
                label: "1".to_string(),
            },
            MediaEntry {
                id: "2".to_string(),
                label: "2".to_string(),
            },
            MediaEntry {
                id: "3".to_string(),
                label: "3".to_string(),
            },
        ];

        let consumed_count = Arc::new(AtomicU32::new(0));
        let consumed_count_clone = consumed_count.clone();

        let res = run_media_loop(
            "Test Anime",
            items,
            None,
            None,
            true, // auto_play_next: starts at ep 1 with skip_selection = true
            true, // binge: should auto play all 3 episodes
            &mut history,
            &history_path,
            Translation::Sub,
            Provider::Senshi,
            MediaLoopConfig {
                select_prompt: "Select",
                use_fuzzy: false,
                unit_plural: "episodes",
                unit_singular: "episode",
                last_verb: "watched",
            },
            |_entry, _ctx| {
                let count = consumed_count_clone.clone();
                async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    Ok(ConsumeOutcome::Consumed)
                }
            },
        )
        .await;

        let _ = std::fs::remove_file(history_path);

        assert!(res.is_ok());
        assert_eq!(consumed_count.load(Ordering::SeqCst), 3);
    }
}


