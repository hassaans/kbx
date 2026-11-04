use std::collections::{BTreeMap, BTreeSet, HashSet};

use kbx_core::Graph;

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
            let Some(weight) = weight else { continue };
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
        Ok(Snapshot {
            notes,
            neighbors,
            forward,
            hubs,
            linked,
        })
    }

    /// Any edge joins the two notes, in either direction.
    pub(crate) fn linked(&self, a: &str, b: &str) -> bool {
        self.linked.contains(&ordered(a, b))
    }

    /// The typed degree of a note.
    pub(crate) fn degree(&self, id: &str) -> usize {
        self.neighbors.get(id).map_or(0, BTreeSet::len)
    }
}

/// The pair with the lower id first.
pub(crate) fn ordered(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
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
