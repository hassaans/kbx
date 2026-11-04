use std::collections::{HashMap, HashSet};

use kbx_core::{Graph, NONE, Review, ordered};
use rusqlite::params;

use crate::pipeline::{Budget, Proposal, suggest};
use crate::prior;
use crate::{GenError, Generator, Scope};

/// How one generator did against `reviews`.
#[derive(Debug, Clone, PartialEq)]
pub struct Score {
    /// The generator.
    pub name: String,
    /// Pairs it found.
    pub proposed: usize,
    /// Those someone accepted or rejected. The rest say nothing either way,
    /// since nobody has looked at them.
    pub reviewed: usize,
    /// Those with an accept for the pair and relation. A candidate no judge
    /// gave a relation matches on the pair.
    pub hits: usize,
    /// `hits` over `reviewed`. `None` if nothing it found was reviewed.
    pub precision: Option<f64>,
    /// `hits` over the accepted pairs. `None` if there are none.
    pub recall: Option<f64>,
}

/// What [`evaluate`] came to.
#[derive(Debug)]
pub struct Evaluation {
    /// One per generator that ran.
    pub scores: Vec<Score>,
    /// Accepted pairs with both notes in the index.
    pub accepted: usize,
    /// Rejected pairs with both notes in the index and no accept.
    pub rejected: usize,
    /// Generators that failed, by name, and why.
    pub skipped: Vec<(String, GenError)>,
}

/// Scores each of `names` on what `proposals` credit it with. `judged` holds the
/// relation of each judged candidate, by cid. Deferrals are no label.
pub fn score(
    names: &[&str],
    proposals: &[Proposal],
    judged: &HashMap<String, String>,
    reviews: &[Review],
) -> Vec<Score> {
    let labels = Labels::new(reviews);
    let rate = |part: usize, whole: usize| (whole > 0).then(|| part as f64 / whole as f64);
    let score_of = |name: &&str| {
        let found = proposals
            .iter()
            .filter(|proposal| proposal.generators.iter().any(|g| g == name));
        let (mut proposed, mut reviewed, mut hits) = (0, 0, 0);
        for proposal in found {
            proposed += 1;
            let pair = ordered(&proposal.src, &proposal.dst);
            reviewed += labels.reviewed.contains(&pair) as usize;
            let rel = judged.get(&proposal.cid).map(String::as_str);
            hits += labels.hit(&pair, rel) as usize;
        }
        Score {
            name: name.to_string(),
            proposed,
            reviewed,
            hits,
            precision: rate(hits, reviewed),
            recall: rate(hits, labels.accepted.len()),
        }
    };
    names.iter().map(score_of).collect()
}

/// `reviews` by pair, lower id first.
struct Labels<'a> {
    /// The relations accepted for each pair.
    accepted: HashMap<(String, String), HashSet<&'a str>>,
    /// Every pair accepted or rejected.
    reviewed: HashSet<(String, String)>,
}

impl<'a> Labels<'a> {
    fn new(reviews: &'a [Review]) -> Labels<'a> {
        let mut accepted: HashMap<_, HashSet<&str>> = HashMap::new();
        let mut reviewed = HashSet::new();
        for review in reviews {
            let pair = ordered(&review.src, &review.dst);
            match review.decision.as_str() {
                "accept" => {
                    accepted
                        .entry(pair.clone())
                        .or_default()
                        .insert(&review.rel);
                    reviewed.insert(pair);
                }
                "reject" => {
                    reviewed.insert(pair);
                }
                _ => {}
            }
        }
        Labels { accepted, reviewed }
    }

    fn hit(&self, pair: &(String, String), rel: Option<&str>) -> bool {
        self.accepted
            .get(pair)
            .is_some_and(|accepted| rel.is_none_or(|rel| accepted.contains(rel)))
    }
}

/// Runs `generators` over the vault as it was before anyone reviewed it, and
/// scores them. `g` isn't changed.
///
/// Accepting a pair links it and deletes its candidate, and rejecting it enough
/// suppresses it, so a generator run on the index as it stands can't find a
/// single reviewed pair. The run gets a temporary copy instead, without the
/// links between accepted pairs, strikes or `kbx.reviewed` locks, and no
/// budget: the point is what each generator can find.
///
/// `type_pair_prior` is left out. It finds no pairs of its own, and it learned
/// from the very rows it would be scored on.
pub fn evaluate(
    g: &Graph,
    mut generators: Vec<Box<dyn Generator>>,
    scope: &Scope<'_>,
) -> Result<Evaluation, GenError> {
    generators.retain(|generator| generator.name() != prior::NAME);
    let dir = tempfile::tempdir()?;
    let held_out = g.copy_to(&dir.path().join("graph.sqlite"))?;
    hold_out(&held_out)?;
    let unlimited = Budget {
        per_note: usize::MAX,
        per_run: usize::MAX,
    };
    let run = suggest(&held_out, &generators, scope, unlimited)?;

    let reviews = known_reviews(g)?;
    let failed = |name: &&str| run.skipped.iter().any(|(skipped, _)| skipped == name);
    let names: Vec<&str> = generators
        .iter()
        .map(|generator| generator.name())
        .collect();
    let names: Vec<&str> = names.into_iter().filter(|name| !failed(name)).collect();
    let labels = Labels::new(&reviews);
    Ok(Evaluation {
        scores: score(&names, &run.candidates, &judged_relations(g)?, &reviews),
        accepted: labels.accepted.len(),
        rejected: labels.reviewed.len() - labels.accepted.len(),
        skipped: run.skipped,
    })
}

/// Takes back what reviewing changed, in the copy.
fn hold_out(copy: &Graph) -> rusqlite::Result<()> {
    let connection = copy.connection();
    for table in ["edges", "links"] {
        connection.execute(
            &format!(
                "DELETE FROM {table} WHERE EXISTS (SELECT 1 FROM reviews r
                     WHERE r.decision = 'accept'
                     AND ((r.src = {table}.src AND r.dst = {table}.dst)
                         OR (r.src = {table}.dst AND r.dst = {table}.src)))"
            ),
            [],
        )?;
    }
    connection.execute("UPDATE reviews SET strikes = 0", [])?;
    // An older database, opened read-only, doesn't have the table.
    connection.execute("DROP TABLE IF EXISTS locks", [])?;
    Ok(())
}

/// The reviews a generator could have found: both notes are here. Under path
/// ACLs (§1), or after importing from another vault, some may not be.
fn known_reviews(g: &Graph) -> rusqlite::Result<Vec<Review>> {
    let mut statement = g.connection().prepare("SELECT id FROM files")?;
    let rows = statement.query_map([], |row| row.get(0))?;
    let notes = rows.collect::<rusqlite::Result<HashSet<String>>>()?;
    let mut reviews = g.reviews()?;
    reviews.retain(|review| notes.contains(&review.src) && notes.contains(&review.dst));
    Ok(reviews)
}

/// The relation of each candidate's latest verdict that names one, which is
/// the one `kbx accept` would take.
fn judged_relations(g: &Graph) -> rusqlite::Result<HashMap<String, String>> {
    let mut statement = g.connection().prepare(
        "SELECT cid, rel FROM verdicts WHERE rel IS NOT NULL AND rel <> ?
         ORDER BY created_at, rowid",
    )?;
    let rows = statement.query_map(params![NONE], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}
