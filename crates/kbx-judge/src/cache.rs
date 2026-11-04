use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use tracing::debug;

use crate::{Availability, JudgeId};

/// Where the probe results live, from the vault root.
pub const CACHE_FILE: &str = ".kbx/judge-cache.json";

/// How long a successful probe stands.
const TTL_SECONDS: i64 = 24 * 60 * 60;

/// One probe's result. Nothing else belongs in this file: never a token.
#[derive(Serialize, Deserialize)]
struct Entry {
    availability: Availability,
    checked_at: i64,
}

type Entries = BTreeMap<String, Entry>;

/// Remembers probes that worked, so a working login costs one call a day.
///
/// A failed probe isn't kept. It spends no tokens, and keeping it would leave
/// kbx saying `NotLoggedIn` for up to a day after the user logs in or the
/// network comes back. A cache that can't be read is an empty one.
#[derive(Debug, Clone)]
pub struct ProbeCache {
    path: Option<PathBuf>,
}

impl ProbeCache {
    /// The cache of the vault at `vault`. Without a vault, nothing is kept.
    pub fn new(vault: Option<&Path>) -> ProbeCache {
        ProbeCache {
            path: vault.map(|vault| vault.join(CACHE_FILE)),
        }
    }

    /// Whether a probe of `id` worked within the last day.
    pub(crate) fn ready(&self, id: JudgeId) -> bool {
        let entries = self.read();
        entries.get(id.as_str()).is_some_and(|entry| {
            let age = now() - entry.checked_at;
            entry.availability == Availability::Ready && (0..TTL_SECONDS).contains(&age)
        })
    }

    /// Records that a probe of `id` worked.
    pub(crate) fn remember(&self, id: JudgeId) {
        let mut entries = self.read();
        let entry = Entry {
            availability: Availability::Ready,
            checked_at: now(),
        };
        entries.insert(id.as_str().to_string(), entry);
        self.write(&entries);
    }

    /// Drops what's known of `id`, whose login has stopped working.
    pub(crate) fn forget(&self, id: JudgeId) {
        let mut entries = self.read();
        if entries.remove(id.as_str()).is_some() {
            self.write(&entries);
        }
    }

    fn read(&self) -> Entries {
        let text = self.path.as_ref().and_then(|p| fs::read_to_string(p).ok());
        text.and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// A cache that can't be written costs a probe, not a failure.
    fn write(&self, entries: &Entries) {
        let Some(path) = &self.path else { return };
        if let Err(error) = replace(path, entries) {
            debug!("the probe cache can't be written: {error}");
        }
    }
}

/// Swaps in the new file in one step, so a crash can't leave half of one.
fn replace(path: &Path, entries: &Entries) -> io::Result<()> {
    let folder = path.parent().unwrap_or(Path::new("."));
    let mut file = NamedTempFile::new_in(folder)?;
    file.write_all(serde_json::to_string_pretty(entries)?.as_bytes())?;
    file.persist(path)?;
    Ok(())
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn vault() -> (TempDir, ProbeCache) {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".kbx")).unwrap();
        let cache = ProbeCache::new(Some(dir.path()));
        (dir, cache)
    }

    fn write_entry(dir: &TempDir, availability: &str, checked_at: i64) {
        let text = format!(
            r#"{{"claude-cli": {{"availability": "{availability}", "checked_at": {checked_at}}}}}"#
        );
        fs::write(dir.path().join(CACHE_FILE), text).unwrap();
    }

    #[test]
    fn a_probe_stands_for_a_day_per_backend() {
        let (dir, cache) = vault();
        assert!(!cache.ready(JudgeId::ClaudeCli));
        cache.remember(JudgeId::ClaudeCli);
        assert!(cache.ready(JudgeId::ClaudeCli));
        assert!(!cache.ready(JudgeId::CodexCli));

        write_entry(&dir, "ready", now() - TTL_SECONDS + 60);
        assert!(cache.ready(JudgeId::ClaudeCli));
        write_entry(&dir, "ready", now() - TTL_SECONDS - 60);
        assert!(!cache.ready(JudgeId::ClaudeCli));
    }

    #[test]
    fn the_file_holds_an_availability_and_a_timestamp_and_nothing_else() {
        let (dir, cache) = vault();
        cache.remember(JudgeId::CodexCli);
        let text = fs::read_to_string(dir.path().join(CACHE_FILE)).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let entry = value["codex-cli"].as_object().unwrap();
        let mut keys: Vec<_> = entry.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["availability", "checked_at"]);
    }

    #[test]
    fn forgetting_leaves_other_backends_alone() {
        let (_dir, cache) = vault();
        cache.remember(JudgeId::ClaudeCli);
        cache.remember(JudgeId::CodexCli);
        cache.forget(JudgeId::ClaudeCli);
        assert!(!cache.ready(JudgeId::ClaudeCli));
        assert!(cache.ready(JudgeId::CodexCli));
    }

    #[test]
    fn a_damaged_cache_or_a_timestamp_from_the_future_counts_for_nothing() {
        let (dir, cache) = vault();
        fs::write(dir.path().join(CACHE_FILE), "not json").unwrap();
        assert!(!cache.ready(JudgeId::ClaudeCli));
        cache.remember(JudgeId::ClaudeCli);
        assert!(cache.ready(JudgeId::ClaudeCli));

        write_entry(&dir, "ready", now() + 3600);
        assert!(!cache.ready(JudgeId::ClaudeCli));
    }

    #[test]
    fn only_a_ready_entry_counts() {
        let (dir, cache) = vault();
        write_entry(&dir, "not_logged_in", now());
        assert!(!cache.ready(JudgeId::ClaudeCli));
    }

    #[test]
    fn without_a_vault_nothing_is_kept() {
        let cache = ProbeCache::new(None);
        cache.remember(JudgeId::ClaudeCli);
        assert!(!cache.ready(JudgeId::ClaudeCli));
    }
}
