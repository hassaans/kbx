use kbx_core::Rates;

use crate::batch::CHARS_PER_TOKEN;
use crate::{CandidateView, JudgeContext, view_tokens};

/// About what a verdict adds to the output: a rationale of up to 40 words and
/// the clamped fields.
const OUTPUT_TOKENS_PER_PAIR: i64 = 100;

/// `batch` at `rates`, with nothing served from a cache.
pub(crate) fn quote(rates: &Rates, batch: &[CandidateView], ctx: &JudgeContext) -> f64 {
    let pairs: i64 = batch.iter().map(view_tokens).sum();
    let input = pairs + (ctx.system.len() / CHARS_PER_TOKEN) as i64;
    rates.price(input, 0, batch.len() as i64 * OUTPUT_TOKENS_PER_PAIR)
}

/// What a run has spent, and whether it may spend more.
///
/// A call's cost is only known once it's made, so a limit has to go by a
/// forecast: the dearest pair of this run so far, or the backend's quote until
/// a call has been priced.
#[derive(Debug, Default)]
pub(crate) struct Ledger {
    limit: Option<f64>,
    spent: Option<f64>,
    dearest_pair: Option<f64>,
}

impl Ledger {
    /// A ledger that opens with `spent`, what picking a backend cost.
    pub(crate) fn new(limit: Option<f64>, spent: f64) -> Ledger {
        Ledger {
            limit,
            spent: (spent > 0.0).then_some(spent),
            dearest_pair: None,
        }
    }

    /// Whether a call on `pairs` pairs should stay within the limit.
    pub(crate) fn allows(&self, pairs: usize, quote: f64) -> bool {
        let Some(limit) = self.limit else { return true };
        let forecast = self.dearest_pair.map_or(quote, |cost| cost * pairs as f64);
        self.spent.unwrap_or(0.0) + forecast <= limit
    }

    /// Records a finished call, which may have failed. `None` means it wasn't
    /// priced.
    pub(crate) fn record(&mut self, pairs: usize, cost: Option<f64>) {
        let Some(cost) = cost else { return };
        *self.spent.get_or_insert(0.0) += cost;
        let per_pair = cost / pairs as f64;
        self.dearest_pair = Some(self.dearest_pair.map_or(per_pair, |d| d.max(per_pair)));
    }

    /// What the run has spent, if any call was priced.
    pub(crate) fn spent(&self) -> Option<f64> {
        self.spent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_limit_anything_goes() {
        assert!(Ledger::new(None, 0.0).allows(1000, 1000.0));
    }

    #[test]
    fn the_quote_decides_until_a_call_has_been_priced() {
        let mut ledger = Ledger::new(Some(0.10), 0.0);
        assert!(ledger.allows(10, 0.10));
        assert!(!ledger.allows(10, 0.11));

        ledger.record(10, None);
        assert!(!ledger.allows(10, 0.11));

        // Now it's known to be far cheaper than quoted.
        ledger.record(10, Some(0.01));
        assert!(ledger.allows(10, 0.11));
    }

    #[test]
    fn the_forecast_is_the_dearest_pair_so_far() {
        let mut ledger = Ledger::new(Some(0.10), 0.0);
        ledger.record(2, Some(0.04));
        ledger.record(10, Some(0.01));
        assert_eq!(ledger.spent(), Some(0.05));
        // 2 pairs at $0.02 fit in the $0.05 left; 3 don't.
        assert!(ledger.allows(2, 0.0));
        assert!(!ledger.allows(3, 0.0));
    }

    #[test]
    fn what_was_spent_before_the_first_call_counts() {
        let ledger = Ledger::new(Some(0.10), 0.08);
        assert_eq!(ledger.spent(), Some(0.08));
        assert!(ledger.allows(10, 0.02));
        assert!(!ledger.allows(10, 0.03));
    }

    #[test]
    fn a_zero_limit_stops_a_paid_backend_and_not_a_free_one() {
        let ledger = Ledger::new(Some(0.0), 0.0);
        assert!(!ledger.allows(1, 0.001));
        assert!(ledger.allows(1, 0.0));
    }
}
