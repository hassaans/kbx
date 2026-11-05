use std::collections::{BTreeMap, BTreeSet, HashSet};

pub(crate) use kbx_core::ordered;
use kbx_core::{Graph, MENTIONS};

/// The most neighbours a note may have and still have its inline links read.
/// Beyond this it's a list: one note linking to 5,000 is 12.5 million pairs.
const MAX_INLINE_REACH: usize = 200;

/// A note, as far as the generators need it.
pub(crate) struct NoteRow {
    pub(crate) class: String,
    pub(crate) title: String,
    pub(crate) path: String,
    pub(crate) scope: Option<String>,
}

/// An edge that reads forwards, as in `A depends_on X`, not `X required_by A`.
pub(crate) struct Forward {
    pub(crate) src: String,
    pub(crate) dst: String,
    pub(crate) rel: String,
    pub(crate) weight: f64,
}

/// The notes and edges of the index, in memory.
pub(crate) struct Snapshot {
    pub(crate) notes: BTreeMap<String, NoteRow>,
    /// Each note's neighbours by typed edges, which are those with a weight.
    /// Mirror rows make this symmetric.
    pub(crate) neighbors: BTreeMap<String, BTreeSet<String>>,
    pub(crate) forward: Vec<Forward>,
    /// Notes with typed degree above p99 (§5).
    pub(crate) hubs: HashSet<String>,
    /// Each note's neighbours by inline links alone, whichever way they point.
    /// Kept apart from `neighbors` so that a typed pair scores as it would
    /// without them, and `degree` still means what `qmd_knn` sorts by.
    pub(crate) inline: BTreeMap<String, BTreeSet<String>>,
    /// Inline links as `(src, dst)`, like `inline` without those of a hub.
    pub(crate) mentions: Vec<(String, String)>,
    linked: HashSet<(String, String)>,
}

impl Snapshot {
    pub(crate) fn load(g: &Graph) -> rusqlite::Result<Snapshot> {
        let connection = g.connection();
        let mut statement = connection.prepare("SELECT id, type, title, path, scope FROM files")?;
        let rows = statement.query_map([], |row| {
            let note = NoteRow {
                class: row.get(1)?,
                title: row.get(2)?,
                path: row.get(3)?,
                scope: row.get(4)?,
            };
            Ok((row.get::<_, String>(0)?, note))
        })?;
        let notes = rows.collect::<rusqlite::Result<BTreeMap<_, _>>>()?;

        let mut neighbors: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut forward = Vec::new();
        let mut mentions = Vec::new();
        let mut linked = HashSet::new();
        let mut statement =
            connection.prepare("SELECT src, dst, rel, inverse_of, weight FROM edges")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let (src, dst, rel): (String, String, String) = (row.get(0)?, row.get(1)?, row.get(2)?);
            let inverse_of: Option<String> = row.get(3)?;
            let weight: Option<f64> = row.get(4)?;
            if src == dst {
                continue;
            }
            linked.insert(ordered(&src, &dst));
            let Some(weight) = weight else {
                if rel == MENTIONS {
                    mentions.push((src, dst));
                }
                continue;
            };
            neighbors
                .entry(src.clone())
                .or_default()
                .insert(dst.clone());
            // The mirror of a symmetric relation reads forwards too.
            if inverse_of.is_none_or(|name| name == rel) {
                forward.push(Forward {
                    src,
                    dst,
                    rel,
                    weight,
                });
            }
        }

        let degrees = notes
            .keys()
            .map(|id| neighbors.get(id).map_or(0, BTreeSet::len));
        let cutoff = percentile_99(degrees.collect());
        let hubs = neighbors
            .iter()
            .filter(|(_, set)| set.len() as f64 > cutoff)
            .map(|(id, _)| id.clone())
            .collect();
        let mut snapshot = Snapshot {
            notes,
            neighbors,
            forward,
            hubs,
            inline: BTreeMap::new(),
            mentions,
            linked,
        };
        snapshot.settle_inline();
        Ok(snapshot)
    }

    /// Fills in `inline` from `mentions`, then drops the inline links of each
    /// hub: a typed hub, or a note with [`reach`](Snapshot::reach) above p99 or
    /// [`MAX_INLINE_REACH`]. Such a note is a list, at either end of a pair.
    fn settle_inline(&mut self) {
        let typed = |a: &str, b: &str| self.neighbors.get(a).is_some_and(|set| set.contains(b));
        let mut inline: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (src, dst) in &self.mentions {
            if !typed(src, dst) {
                inline.entry(src.clone()).or_default().insert(dst.clone());
                inline.entry(dst.clone()).or_default().insert(src.clone());
            }
        }
        self.inline = inline;
        let reaches = self.notes.keys().map(|id| self.reach(id));
        let cutoff = percentile_99(reaches.collect()).min(MAX_INLINE_REACH as f64);
        let above = self
            .inline
            .keys()
            .filter(|id| self.reach(id) as f64 > cutoff);
        let mut hubs: HashSet<String> = above.cloned().collect();
        hubs.extend(self.hubs.iter().cloned());

        self.inline.retain(|id, _| !hubs.contains(id));
        for set in self.inline.values_mut() {
            set.retain(|id| !hubs.contains(id));
        }
        self.mentions
            .retain(|(src, dst)| !hubs.contains(src) && !hubs.contains(dst));
    }

    /// Forgets inline links, apart from that they link their notes.
    pub(crate) fn without_inline(mut self) -> Snapshot {
        self.inline.clear();
        self.mentions.clear();
        self
    }

    /// Any edge joins the two notes, in either direction.
    pub(crate) fn linked(&self, a: &str, b: &str) -> bool {
        self.linked.contains(&ordered(a, b))
    }

    /// The typed degree of a note.
    pub(crate) fn degree(&self, id: &str) -> usize {
        self.neighbors.get(id).map_or(0, BTreeSet::len)
    }

    /// The degree of a note, by typed edges and inline links.
    pub(crate) fn reach(&self, id: &str) -> usize {
        self.degree(id) + self.inline.get(id).map_or(0, BTreeSet::len)
    }
}

/// Interpolates between ranks. Under 100 notes the nearest rank is the
/// maximum, and nothing could ever be above it.
fn percentile_99(mut degrees: Vec<usize>) -> f64 {
    degrees.sort_unstable();
    let Some(last) = degrees.len().checked_sub(1) else {
        return 0.0;
    };
    let rank = 0.99 * last as f64;
    let below = rank.floor() as usize;
    let above = (below + 1).min(last);
    let low = degrees[below] as f64;
    low + (degrees[above] as f64 - low) * (rank - below as f64)
}

#[cfg(test)]
mod tests {
    use super::percentile_99;

    #[test]
    fn the_top_node_of_a_small_graph_can_be_above_p99() {
        assert!(5.0 > percentile_99(vec![1, 1, 2, 2, 5]));
    }

    #[test]
    fn equal_degrees_leave_nothing_above_p99() {
        assert_eq!(percentile_99(vec![3, 3, 3]), 3.0);
    }

    #[test]
    fn an_empty_graph_has_a_cutoff() {
        assert_eq!(percentile_99(Vec::new()), 0.0);
    }
}
