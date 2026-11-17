//! Discover exported replay bundles without decompressing them on the UI thread.
use std::path::{Path, PathBuf};

pub(super) struct Schedule {
    pub path: String,
    pub label: String,
    rank: u8,
}

fn entry(path: &Path) -> Schedule {
    let path = path.to_string_lossy().into_owned();
    let (rank, label) = if path
        .ends_with("worldview60-cadence-demand-matched-24h-20261116/best-aligned-c2-replay.json.gz")
    {
        (0, "Aligned deadlines · AOI strips (84.88% met)".to_owned())
    } else if path
        .ends_with("worldview60-strips-comparison-buffered-20261116/best-c2-replay.json.gz")
    {
        (
            1,
            "Randomized deadlines · AOI strips (73.99% met)".to_owned(),
        )
    } else if path.ends_with("assets/schedules/worldview60-ongoing-baseline-24h.json.gz") {
        (2, "Original baseline (58.57% met)".to_owned())
    } else {
        let p = Path::new(&path);
        let name = p.file_name().unwrap_or_default().to_string_lossy();
        let parent = p
            .parent()
            .and_then(Path::file_name)
            .unwrap_or_default()
            .to_string_lossy();
        (3, format!("{parent} / {name}"))
    };
    Schedule { path, label, rank }
}

fn scan(dir: &Path, depth: usize, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for item in entries.flatten() {
        let Ok(kind) = item.file_type() else {
            continue;
        };
        let path = item.path();
        if kind.is_dir() && depth > 0 {
            scan(&path, depth - 1, files);
        } else if kind.is_file() {
            let name = item.file_name();
            let name = name.to_string_lossy();
            if (name.ends_with(".json") || name.ends_with(".json.gz"))
                && (name.contains("replay")
                    || name.contains("playback")
                    || depth == 0 && dir.ends_with("assets/schedules"))
            {
                files.push(path);
            }
        }
    }
}

pub(super) fn discover(root: &Path, supplied: Option<&str>) -> Vec<Schedule> {
    let mut files = Vec::new();
    scan(&root.join("assets/schedules"), 0, &mut files);
    scan(&root.join("artifacts/scheduling-solutions"), 3, &mut files);
    if let Some(path) = supplied {
        files.push(PathBuf::from(path));
    }
    // The bundled baseline is a copy of this export; present it once.
    if root
        .join("assets/schedules/worldview60-ongoing-baseline-24h.json.gz")
        .is_file()
    {
        files.retain(|p| {
            !p.ends_with("worldview60-ongoing-baseline-24h-20261113/playback.json.gz")
                || supplied.is_some_and(|s| p == Path::new(s))
        });
    }
    files.sort();
    files.dedup();
    let mut entries: Vec<_> = files.iter().map(|p| entry(p)).collect();
    entries.sort_by(|a, b| {
        a.rank
            .cmp(&b.rank)
            .then_with(|| a.label.cmp(&b.label))
            .then_with(|| a.path.cmp(&b.path))
    });
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_replays_prefers_aligned_and_keeps_external_selection() {
        let root = std::env::temp_dir().join(format!(
            "leo-schedule-picker-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for path in [
            "assets/schedules/worldview60-ongoing-baseline-24h.json.gz",
            "artifacts/scheduling-solutions/worldview60-cadence-demand-matched-24h-20261116/best-aligned-c2-replay.json.gz",
            "artifacts/scheduling-solutions/worldview60-ongoing-baseline-24h-20261113/playback.json.gz",
            "artifacts/scheduling-solutions/run/playback.json",
            "artifacts/scheduling-solutions/run/schedule.json",
            "artifacts/scheduling-solutions/run/report.json",
        ] {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"unparsed bundle").unwrap();
        }
        let entries = discover(&root, None);
        assert_eq!(entries.len(), 3);
        assert!(entries[0].label.starts_with("Aligned deadlines"));
        assert!(entries[1].label.starts_with("Original baseline"));
        assert_eq!(discover(&root, Some(&entries[0].path)).len(), 3);
        let external = root.join("external.json.gz").to_string_lossy().into_owned();
        assert!(discover(&root, Some(&external))
            .iter()
            .any(|s| s.path == external));
        std::fs::remove_dir_all(root).unwrap();
    }
}
