use anyhow::Result;
use futures::future::join_all;
use std::time::Duration;

use crate::providers::AnimeProvider;
use crate::types::{Provider, ShowInfo, Translation};

/// Search every anime provider concurrently for `query` and merge the results
/// into `(provider, show)` pairs. Clients are constructed per call; a provider
/// that fails to initialize (or times out) contributes no results. A warning is
/// printed when a majority of the attempted providers time out.
pub async fn aggregate_anime_search(
    query: &str,
    translation: Translation,
    search_timeout: Duration,
    timeout_secs: u64,
) -> Result<Vec<(Provider, ShowInfo)>> {
    println!("Searching across all anime providers for \"{}\"...", query);

    let clients = Provider::all_anime()
        .iter()
        .map(|p| (*p, p.anime_client().ok()))
        .collect::<Vec<_>>();

    let attempted_count = clients.iter().filter(|(_, c)| c.is_some()).count();

    let futures = clients.into_iter().map(|(provider, client)| async move {
        let (shows, timed_out) = match client {
            Some(client) => {
                let fut = client.search_shows(query, translation);
                match tokio::time::timeout(search_timeout, fut).await {
                    Ok(Ok(items)) => (items, false),
                    Ok(Err(_)) => (Vec::new(), false),
                    Err(_) => (Vec::new(), true),
                }
            }
            None => (Vec::new(), false),
        };
        (provider, shows, timed_out)
    });

    let mut combined = Vec::new();
    let mut timed_out_count = 0;
    for (provider, shows, timed_out) in join_all(futures).await {
        if timed_out {
            timed_out_count += 1;
        }
        for show in shows {
            combined.push((provider, show));
        }
    }

    if attempted_count > 0 && timed_out_count * 2 >= attempted_count {
        eprintln!(
            "Warning: {} of {} providers timed out after {}s. Try increasing search timeout with `-T <seconds>` (e.g. `-T 30`).",
            timed_out_count, attempted_count, timeout_secs
        );
    }

    Ok(combined)
}


