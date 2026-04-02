use std::io::IsTerminal;

use anyhow::{Result, anyhow};
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};

use crate::{config::AppConfig, picker, scanner::ProjectEntry, scanner::load_or_scan_projects};

pub struct GoResult {
    pub path: String,
    /// Time spent waiting for interactive picker input (should be excluded from perf stats)
    pub interactive_ms: u128,
}

pub fn execute(config: &AppConfig, query: &str) -> Result<GoResult> {
    let cache = load_or_scan_projects(config)?;
    let matches = rank_matches(&cache.projects, query);

    if matches.is_empty() {
        return Err(anyhow!(
            "No project matching '{query}' found. Run `qr scan` to refresh."
        ));
    }

    let mut interactive_ms: u128 = 0;
    let selected = if matches.len() == 1 {
        matches[0].clone()
    } else if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        let labels = matches
            .iter()
            .map(|entry| format!("{} ({})", entry.name, entry.path))
            .collect::<Vec<_>>();
        let picker_start = std::time::Instant::now();
        let Some(index) = picker::pick_index(&labels)? else {
            return Err(anyhow!("Selection cancelled"));
        };
        interactive_ms = picker_start.elapsed().as_millis();
        matches[index].clone()
    } else {
        return Err(anyhow!(
            "Multiple matches for '{query}': {}",
            matches
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    };

    Ok(GoResult {
        path: selected.path,
        interactive_ms,
    })
}

pub fn rank_matches(entries: &[ProjectEntry], query: &str) -> Vec<ProjectEntry> {
    let lower_query = query.to_ascii_lowercase();
    let matcher = SkimMatcherV2::default();

    let mut exact = entries
        .iter()
        .filter(|entry| entry.name.eq_ignore_ascii_case(&lower_query))
        .cloned()
        .collect::<Vec<_>>();
    if !exact.is_empty() {
        exact.sort_by(|a, b| a.path.cmp(&b.path));
        return exact;
    }

    let mut scored = entries
        .iter()
        .filter_map(|entry| {
            let score = if entry.name.to_ascii_lowercase().contains(&lower_query) {
                Some((10_000 - entry.name.len() as i64) + lower_query.len() as i64)
            } else if entry.path.to_ascii_lowercase().contains(&lower_query) {
                Some(5_000 - entry.path.len() as i64)
            } else {
                matcher.fuzzy_match(&entry.name, query)
            }?;
            Some((score, entry))
        })
        .collect::<Vec<_>>();

    scored.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.name.cmp(&right.1.name)));
    let results: Vec<ProjectEntry> = scored
        .into_iter()
        .take(9)
        .map(|(_, entry)| entry.clone())
        .collect();

    // If strict matching found nothing, try bigram similarity for typos/transpositions
    if results.is_empty() && lower_query.len() >= 3 {
        let query_bigrams = bigrams(&lower_query);
        let mut fallback: Vec<(f64, &ProjectEntry)> = entries
            .iter()
            .filter_map(|entry| {
                let name_lower = entry.name.to_ascii_lowercase();
                let name_bigrams = bigrams(&name_lower);
                let sim = bigram_similarity(&query_bigrams, &name_bigrams);
                if sim >= 0.25 {
                    Some((sim, entry))
                } else {
                    None
                }
            })
            .collect();
        fallback.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        return fallback
            .into_iter()
            .take(9)
            .map(|(_, entry)| entry.clone())
            .collect();
    }

    results
}

fn bigrams(s: &str) -> Vec<(char, char)> {
    let chars: Vec<char> = s.chars().collect();
    chars.windows(2).map(|w| (w[0], w[1])).collect()
}

fn bigram_similarity(a: &[(char, char)], b: &[(char, char)]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut b_remaining: Vec<bool> = vec![true; b.len()];
    let mut matches = 0;
    for pair in a {
        for (i, other) in b.iter().enumerate() {
            if b_remaining[i] && pair == other {
                b_remaining[i] = false;
                matches += 1;
                break;
            }
        }
    }
    (2.0 * matches as f64) / (a.len() + b.len()) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_projects() -> Vec<ProjectEntry> {
        vec![
            ProjectEntry {
                name: "orion-app".into(),
                path: "/dev/orion-app".into(),
                source: "git".into(),
            },
            ProjectEntry {
                name: "orion-api".into(),
                path: "/dev/orion-api".into(),
                source: "git".into(),
            },
            ProjectEntry {
                name: "quick-runner".into(),
                path: "/dev/quick-runner".into(),
                source: "git".into(),
            },
        ]
    }

    #[test]
    fn exact_match_wins() {
        let matches = rank_matches(&sample_projects(), "quick-runner");
        assert_eq!(matches[0].name, "quick-runner");
        assert_eq!(matches.len(), 1);
    }

    #[test]
    fn fuzzy_matches_are_ranked() {
        let matches = rank_matches(&sample_projects(), "orion");
        assert_eq!(matches.len(), 2);
        assert!(matches.iter().all(|entry| entry.name.starts_with("orion")));
    }
}
