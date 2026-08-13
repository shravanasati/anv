use anyhow::{Result, bail};
use std::cmp::Ordering;
use std::future::Future;
use std::time::Duration;

pub fn parse_episode_key(label: &str) -> f64 {
    label.parse::<f64>().unwrap_or(0.0)
}

pub fn sorted_episode_labels(episodes: &[String]) -> Vec<String> {
    let mut sorted = episodes.to_vec();
    sorted.sort_by(|a, b| {
        parse_episode_key(a)
            .partial_cmp(&parse_episode_key(b))
            .unwrap_or(Ordering::Equal)
    });
    sorted.dedup();
    sorted
}

pub fn next_episode_label_presorted(current: &str, sorted: &[String]) -> Option<String> {
    let pos = sorted.iter().position(|ep| ep == current)?;
    sorted.get(pos + 1).cloned()
}

pub async fn search_single_with_timeout<F, T>(
    timeout: Duration,
    provider_name: &str,
    timeout_secs: u64,
    fut: F,
) -> Result<Vec<T>>
where
    F: Future<Output = Result<Vec<T>>>,
{
    match tokio::time::timeout(timeout, fut).await {
        Ok(res) => res,
        Err(_) => bail!(
            "Search on {} timed out after {}s. Try increasing search timeout with `-T <seconds>`.",
            provider_name,
            timeout_secs
        ),
    }
}

pub async fn search_opt_with_timeout<F, T>(
    opt_fut: Option<F>,
    timeout: Duration,
) -> (Vec<T>, bool)
where
    F: Future<Output = Result<Vec<T>>>,
{
    if let Some(fut) = opt_fut {
        match tokio::time::timeout(timeout, fut).await {
            Ok(Ok(items)) => (items, false),
            Ok(Err(_)) => (Vec::new(), false),
            Err(_) => (Vec::new(), true),
        }
    } else {
        (Vec::new(), false)
    }
}
