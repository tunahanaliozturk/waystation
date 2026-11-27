//! Upstream groups and the choice of a server for each request.
//!
//! Two policies, both weighted. Round robin hands out requests in the exact
//! proportion of the weights. Least requests draws two servers at random, in
//! proportion to their weights, and takes the one with fewer requests in
//! flight for its weight, which needs no lock (the "power of two choices").

use std::hash::{BuildHasher, RandomState};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde::Deserialize;

use crate::pool::{Pool, PoolSettings};

/// How a group picks a server.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Policy {
    /// Smooth weighted round robin: a server of weight 3 gets three of every
    /// `total weight` requests, spread out rather than in a row.
    RoundRobin,
    /// Power of two choices, weighted.
    #[default]
    LeastRequests,
}

/// One upstream server and its idle connections.
#[derive(Debug)]
pub(crate) struct Server {
    pub(crate) addr: SocketAddr,
    weight: u64,
    in_flight: AtomicUsize,
    pub(crate) pool: Pool,
}

/// A named set of servers that share the requests of the routes pointing at
/// it.
#[derive(Debug)]
pub(crate) struct Group {
    policy: Policy,
    servers: Vec<Arc<Server>>,
    /// Running sums of the weights, for drawing a server by weight.
    cumulative: Vec<u64>,
    /// The current weights of smooth round robin. Each stays above minus
    /// the total and below the number of servers times the total, which
    /// `i128` holds for any total that fits `u64`.
    // ponytail: one lock per pick for round robin; a precomputed schedule
    // and an atomic index would avoid it if the lock ever shows in a profile.
    current: Mutex<Vec<i128>>,
    random: Random,
}

impl Group {
    /// A group of `servers`, each an address and a weight of at least one.
    /// Returns `None` for an empty list, a weight of zero, or weights that
    /// add up to more than `u64::MAX`.
    pub(crate) fn new(
        policy: Policy,
        servers: &[(SocketAddr, u32)],
        pool: PoolSettings,
    ) -> Option<Self> {
        if servers.is_empty() || servers.iter().any(|&(_, weight)| weight == 0) {
            return None;
        }
        let servers: Vec<Arc<Server>> = servers
            .iter()
            .map(|&(addr, weight)| {
                Arc::new(Server {
                    addr,
                    weight: u64::from(weight),
                    in_flight: AtomicUsize::new(0),
                    pool: Pool::new(pool),
                })
            })
            .collect();
        let cumulative = running_sums(servers.iter().map(|server| server.weight))?;
        Some(Self {
            policy,
            current: Mutex::new(vec![0; servers.len()]),
            servers,
            cumulative,
            random: Random::new(),
        })
    }

    /// Picks a server for one request and counts the request against it
    /// until the lease is dropped.
    pub(crate) fn pick(&self) -> Option<Lease> {
        let index = match (self.servers.len(), self.policy) {
            (1, _) => 0,
            (_, Policy::RoundRobin) => self.next_in_turn(),
            (_, Policy::LeastRequests) => self.less_loaded_of_two(),
        };
        self.servers.get(index).map(Lease::new)
    }

    /// Every server in the group.
    pub(crate) fn servers(&self) -> &[Arc<Server>] {
        &self.servers
    }

    /// Smooth weighted round robin, as nginx does it: every server gains its
    /// weight, the one with the most goes next and pays the total back.
    fn next_in_turn(&self) -> usize {
        let total = self.cumulative.last().copied().unwrap_or(0);
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        let mut best: Option<(usize, i128)> = None;
        for (index, (value, server)) in current.iter_mut().zip(&self.servers).enumerate() {
            *value += i128::from(server.weight);
            if best.is_none_or(|(_, top)| *value > top) {
                best = Some((index, *value));
            }
        }
        let best = best.map_or(0, |(index, _)| index);
        if let Some(value) = current.get_mut(best) {
            *value -= i128::from(total);
        }
        best
    }

    /// Draws two servers by weight and keeps the one with fewer requests in
    /// flight for its weight. On a tie the first draw wins, so with no load
    /// at all the requests follow the weights exactly in expectation.
    fn less_loaded_of_two(&self) -> usize {
        let a = self.draw();
        let b = self.draw();
        let load = |index: usize, other: usize| {
            let in_flight = self
                .servers
                .get(index)
                .map_or(0, |server| server.in_flight.load(Ordering::Relaxed));
            let weight = self.servers.get(other).map_or(1, |server| server.weight);
            // Requests per unit of weight, compared without dividing.
            u128::from(u64::try_from(in_flight).unwrap_or(u64::MAX)) * u128::from(weight)
        };
        if load(b, a) < load(a, b) { b } else { a }
    }

    /// A server index drawn in proportion to the weights.
    fn draw(&self) -> usize {
        let total = self.cumulative.last().copied().unwrap_or(1);
        let point = self.random.next() % total;
        self.cumulative.partition_point(|&sum| sum <= point)
    }
}

/// The running sums of `weights`, or `None` if the total does not fit.
fn running_sums(weights: impl IntoIterator<Item = u64>) -> Option<Vec<u64>> {
    let mut sum: u64 = 0;
    weights
        .into_iter()
        .map(|weight| {
            sum = sum.checked_add(weight)?;
            Some(sum)
        })
        .collect()
}

/// A request counted against a server for as long as it is alive.
#[derive(Debug)]
pub(crate) struct Lease(Arc<Server>);

impl Lease {
    fn new(server: &Arc<Server>) -> Self {
        server.in_flight.fetch_add(1, Ordering::Relaxed);
        Self(Arc::clone(server))
    }

    pub(crate) fn server(&self) -> &Server {
        &self.0
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

/// `SplitMix64` on an atomic counter: lock-free, and plenty for spreading
/// requests. Not for anything that needs to be unpredictable.
#[derive(Debug)]
struct Random(AtomicU64);

impl Random {
    fn new() -> Self {
        Self(AtomicU64::new(RandomState::new().hash_one(0_u8)))
    }

    fn next(&self) -> u64 {
        const GAMMA: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut z = self
            .0
            .fetch_add(GAMMA, Ordering::Relaxed)
            .wrapping_add(GAMMA);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(policy: Policy, weights: &[u32]) -> Group {
        let servers: Vec<(SocketAddr, u32)> = weights
            .iter()
            .zip(1..)
            .map(|(&weight, port)| (SocketAddr::from(([127, 0, 0, 1], port)), weight))
            .collect();
        Group::new(policy, &servers, PoolSettings::default()).unwrap()
    }

    fn index(lease: &Lease) -> usize {
        usize::from(lease.server().addr.port() - 1)
    }

    #[test]
    fn round_robin_spreads_each_cycle_by_weight() {
        let group = group(Policy::RoundRobin, &[5, 1, 1]);
        let order: Vec<usize> = (0..14).map(|_| index(&group.pick().unwrap())).collect();
        // nginx's example: a, a, b, a, c, a, a for weights 5, 1, 1.
        assert_eq!(order, [0, 0, 1, 0, 2, 0, 0, 0, 0, 1, 0, 2, 0, 0]);
    }

    #[test]
    fn least_requests_draws_by_weight_when_nothing_is_in_flight() {
        let group = group(Policy::LeastRequests, &[1, 3]);
        let mut counts = [0_usize; 2];
        for _ in 0..40_000 {
            // Each lease is dropped at once, so nothing is ever in flight.
            counts[index(&group.pick().unwrap())] += 1;
        }
        // 10,000 and 30,000 expected; the spread of a draw is about 90.
        assert!(counts[0].abs_diff(10_000) < 1_000, "{counts:?}");
    }

    #[test]
    fn least_requests_sends_new_requests_away_from_a_loaded_server() {
        let group = group(Policy::LeastRequests, &[1, 1]);
        let busy: Vec<Lease> = (0..20).map(|_| Lease::new(&group.servers()[0])).collect();
        let mut to_busy = 0_usize;
        for _ in 0..10_000 {
            if index(&group.pick().unwrap()) == 0 {
                to_busy += 1;
            }
        }
        // Only when both draws land on it: a quarter of the time.
        assert!(to_busy.abs_diff(2_500) < 500, "{to_busy}");
        drop(busy);
        assert_eq!(group.servers()[0].in_flight.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn load_is_compared_per_unit_of_weight() {
        let group = group(Policy::LeastRequests, &[1, 4]);
        // Three in flight on a server of weight 4 is less load than one on a
        // server of weight 1.
        let _light = Lease::new(&group.servers()[0]);
        let _heavy: Vec<Lease> = (0..3).map(|_| Lease::new(&group.servers()[1])).collect();
        let to_light = (0..10_000)
            .filter(|_| index(&group.pick().unwrap()) == 0)
            .count();
        // Only when both draws land on the light one: (1/5)^2 = 4%.
        assert!(to_light < 800, "{to_light}");
    }

    #[test]
    fn a_group_needs_servers_and_weights_of_at_least_one() {
        let pool = PoolSettings::default();
        assert!(Group::new(Policy::RoundRobin, &[], pool).is_none());
        let addr = SocketAddr::from(([127, 0, 0, 1], 1));
        assert!(Group::new(Policy::RoundRobin, &[(addr, 0)], pool).is_none());
    }

    #[test]
    fn weights_that_add_up_past_the_largest_total_make_no_group() {
        assert_eq!(running_sums([1, 2, 5]), Some(vec![1, 3, 8]));
        assert_eq!(
            running_sums([u64::MAX - 1, 1]),
            Some(vec![u64::MAX - 1, u64::MAX])
        );
        assert_eq!(running_sums([u64::MAX, 1]), None);
    }
}
