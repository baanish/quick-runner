use std::io::IsTerminal;

use anyhow::{Result, anyhow};
use fuzzy_matcher::{FuzzyMatcher, skim::SkimMatcherV2};

use crate::{config::AppConfig, picker, scanner::ProjectEntry, scanner::load_or_scan_projects};

pub struct GoResult {
    pub path: String,
}

pub fn execute(config: &AppConfig, query: &str) -> Result<GoResult> {
    let cache = load_or_scan_projects(config)?;
    let matches = rank_matches(&cache.projects, query);

    if matches.is_empty() {
        return Err(anyhow!(
            "No project matching '{query}' found. Run `qr scan` to refresh."
        ));
    }

    let selected = if matches.len() == 1 {
        matches[0].clone()
    } else if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        let labels = matches
            .iter()
            .map(|entry| format!("{} ({})", entry.name, entry.path))
            .collect::<Vec<_>>();
        let Some(index) = picker::pick_index(&labels)? else {
            return Err(anyhow!("Selection cancelled"));
        };
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
    })
}

pub fn rank_matches(entries: &[ProjectEntry], query: &str) -> Vec<ProjectEntry> {
    let lower_query = query.to_ascii_lowercase();
    let matcher = SkimMatcherV2::default();

    let mut exact = entries
        .iter()
        .filter(|entry| entry.name.to_ascii_lowercase() == lower_query)
        .cloned()
        .collect::<Vec<_>>();
    if !exact.is_empty() {
        exact.sort_by(|a, b| a.path.cmp(&b.path));
        return exact;
    }

    let mut scored = entries
        .iter()
        .filter_map(|entry| {
            let name = entry.name.to_ascii_lowercase();
            let path = entry.path.to_ascii_lowercase();
            let score = if name.contains(&lower_query) {
                Some((10_000 - name.len() as i64) + lower_query.len() as i64)
            } else if path.contains(&lower_query) {
                Some(5_000 - path.len() as i64)
            } else {
                matcher.fuzzy_match(&entry.name, query)
            }?;
            Some((score, entry.clone()))
        })
        .collect::<Vec<_>>();

    scored.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.name.cmp(&right.1.name)));
    scored.into_iter().map(|(_, entry)| entry).take(9).collect()
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
