use std::cmp::Ordering;

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
