//! What holds a read back while the index it names is still being filled.
//!
//! It lives here rather than on `Db` because only this layer can ever open it.
//! Filling an index means deriving rows from objects, and an object is bytes to
//! everything below [`Record`](crate::Record), so a gate in `doclite-core`
//! could park a byte-level caller with nothing in the world able to release it.
//! `Db::building` reports; this waits.
//!
//! The waiters are `futures_channel::oneshot` receivers, which belong to no
//! runtime (ADR 0006). The senders are held *here* and taken by [`Building`],
//! which is what turns a rebuild that did not finish into an error rather than
//! a hang: dropping the senders without firing them wakes every parked read.
//! Why they woke is then a question about the entry they were parked on, which
//! is why [`Filling`] distinguishes a rebuild that failed from one that was
//! dropped. A reader is owed the reason, not just the fact.
//!
//! Nothing holds the lock across an await. [`Gate::waiting`] takes it, collects
//! a receiver per index, and gives it up before any of them is awaited.
//!
//! `armed` is what keeps all of this off the ordinary path. A store whose
//! indexes are all filled holds an empty map for its whole life, and checking
//! one relaxed bool is cheaper than taking the lock and building the index list
//! to look up in it.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use doclite_core::{IndexId, StorageError};
use futures_channel::oneshot;

use crate::error::DocumentError;

type Waiters = BTreeMap<IndexId, Filling>;

/// One index and the channel a read parked on it is waiting on.
type Parked = (IndexId, oneshot::Receiver<()>);

/// Where an index that is not yet readable has got to.
///
/// Absence from the map is the fourth state and means filled. Keeping the other
/// three as entries rather than removals is what stops a read from mistaking
/// "nothing will ever fill this" for "this is complete" and answering out of an
/// index with rows missing.
///
/// [`Filling::Failed`] and [`Filling::Abandoned`] are both terminal and are not
/// the same thing. A rebuild that refused has a reason the caller asked for and
/// is owed; one that was dropped has no reason beyond having been dropped.
/// Collapsing them is what made a taken unique key report itself as a rebuild
/// going away.
enum Filling {
    /// Something is filling it, and these are waiting for it to finish.
    Running(Vec<oneshot::Sender<()>>),
    /// Filling it was tried and refused, for this reason.
    Failed(DocumentError),
    /// Nothing is filling it any more, and nothing in this handle will.
    Abandoned,
}

/// Which indexes are still being filled, and who is parked on each.
///
/// An index present in the map is being filled. Removing it is what releases
/// everyone parked on it, so being filled is the presence of an entry rather
/// than a flag that could disagree with the waiter list beside it.
#[derive(Default)]
pub(crate) struct Gate {
    building: Mutex<Waiters>,
    /// Whether the map holds anything at all.
    ///
    /// Kept beside the map rather than derived from it so the ordinary path
    /// never takes the lock. It is set once at construction and cleared when
    /// the last entry is removed, both of which happen under the lock, so it
    /// cannot drift: every transition that empties the map is the one that
    /// clears this.
    armed: AtomicBool,
}

impl Gate {
    /// A gate over the indexes an engine said it has rows missing from.
    pub(crate) fn over(building: &[IndexId]) -> Self {
        Self {
            armed: AtomicBool::new(!building.is_empty()),
            building: Mutex::new(
                building
                    .iter()
                    .map(|index| (*index, Filling::Running(Vec::new())))
                    .collect(),
            ),
        }
    }

    /// Whether anything here could hold a read back.
    ///
    /// False for the whole life of a store whose indexes are all filled, which
    /// is the case worth making cheap: a caller that checks this first pays one
    /// relaxed load instead of a lock and a list of the indexes it reads.
    pub(crate) fn armed(&self) -> bool {
        self.armed.load(Ordering::Acquire)
    }

    /// Waits until every index this work reads holds all of its rows.
    ///
    /// Takes what the work *reads* and not what it names. Filing a row into an
    /// index whose rows are still being filled is safe, so a caller is never
    /// made to wait for one; reading one is not, and
    /// [`Operation::reads`](doclite_core::Operation::reads) is where that line
    /// is drawn.
    pub(crate) async fn clear(&self, reads: &[IndexId]) -> Result<(), DocumentError> {
        for (index, answer) in self.waiting(reads)? {
            match answer.await {
                Ok(()) => {}
                Err(_woken) => return Err(self.why(index)?),
            }
        }
        Ok(())
    }

    /// Why a parked read was woken without its index being filled.
    ///
    /// The sender was dropped rather than fired, which says only that the
    /// rebuild did not finish. Which of the two ways it did not finish is on the
    /// entry, so this is a second look rather than an assumption. An entry that
    /// has gone entirely means the index was filled between the wake and this
    /// lookup, and the read may proceed.
    fn why(&self, index: IndexId) -> Result<DocumentError, DocumentError> {
        match self.held()?.get(&index) {
            Some(Filling::Failed(error)) => Ok(error.clone()),
            Some(Filling::Abandoned) | Some(Filling::Running(_)) | None => {
                Ok(DocumentError::Rebuilding { index })
            }
        }
    }

    /// One receiver per index that is still being filled, with the lock given
    /// up before any of them is awaited.
    fn waiting(&self, reads: &[IndexId]) -> Result<Vec<Parked>, DocumentError> {
        let mut building = self.held()?;
        if building.is_empty() {
            return Ok(Vec::new());
        }

        let mut waiting = Vec::new();
        for index in reads {
            match building.get_mut(index) {
                None => {}
                Some(Filling::Failed(error)) => return Err(error.clone()),
                Some(Filling::Abandoned) => {
                    return Err(DocumentError::Rebuilding { index: *index })
                }
                Some(Filling::Running(waiters)) => {
                    let (tell, answer) = oneshot::channel();
                    waiters.push(tell);
                    waiting.push((*index, answer));
                }
            }
        }
        Ok(waiting)
    }

    /// Releases everyone parked on this index, because its rows are all there.
    ///
    /// The entry goes, and its absence is what later reads see as filled.
    pub(crate) fn opened(&self, index: IndexId) -> Result<(), DocumentError> {
        let waiters = {
            let mut building = self.held()?;
            let taken = match building.remove(&index) {
                None | Some(Filling::Abandoned) | Some(Filling::Failed(_)) => Vec::new(),
                Some(Filling::Running(waiters)) => waiters,
            };
            self.armed.store(!building.is_empty(), Ordering::Release);
            taken
        };
        for waiter in waiters {
            match waiter.send(()) {
                Ok(()) => {}
                Err(_gone) => {}
            }
        }
        Ok(())
    }

    /// Records why filling this index was refused, waking everyone parked on it.
    ///
    /// The entry stays and keeps the gate armed, because the index still has
    /// rows missing and a later read of it has to be refused too rather than
    /// answered.
    fn failed(&self, index: IndexId, error: &DocumentError) {
        match self.held() {
            Ok(mut building) => {
                if let Some(entry) = building.get_mut(&index) {
                    *entry = Filling::Failed(error.clone());
                }
            }
            Err(_poisoned) => {}
        }
    }

    /// Marks this index as one nothing will fill, dropping the senders without
    /// firing them so every parked read wakes as
    /// [`DocumentError::Rebuilding`].
    ///
    /// The entry stays, as [`Filling::Abandoned`]. Removing it would read as
    /// filled, and a read would then answer out of an index that still has rows
    /// missing, which is the one outcome none of this may produce.
    ///
    /// Called only from [`Building`]'s [`Drop`], which has nowhere to report
    /// to. A poisoned lock is tolerable here and nowhere else: it means a
    /// thread panicked holding it, every sender inside it is being dropped
    /// along with the gate, and that drop is exactly what this was causing.
    fn abandoned(&self, index: IndexId) {
        match self.held() {
            Ok(mut building) => {
                if let Some(entry) = building.get_mut(&index) {
                    *entry = Filling::Abandoned;
                }
            }
            Err(_poisoned) => {}
        }
    }

    fn held(&self) -> Result<MutexGuard<'_, Waiters>, DocumentError> {
        self.building
            .lock()
            .map_err(|_| DocumentError::Storage(StorageError::Poisoned))
    }
}

/// The indexes one rebuild is responsible for, held for as long as it runs.
///
/// A rebuild that finishes calls [`Building::filled`] per index and the guard
/// has nothing left to release. One that refuses calls [`Building::failed`],
/// which keeps the reason so a reader is told what went wrong rather than only
/// that something did. One that is dropped part way, because the spawner
/// discarded the future or the task was cancelled, falls to [`Drop`], which
/// releases every index it still holds *without* firing: the reads parked on
/// them then get an error naming the index instead of waiting for something
/// that will never arrive.
///
/// It owns its share of the gate rather than borrowing one, so that it can be
/// moved into the future handed to a [`Spawner`](crate::Spawner). That is not a
/// detail: a spawner is allowed to drop the work without ever polling it, and a
/// guard built inside the future's body would never have existed to be dropped.
/// Captured state is dropped with an unpolled future, so the guard has to be
/// made before the future and moved in.
pub(crate) struct Building {
    gate: Arc<Gate>,
    held: Vec<IndexId>,
}

impl Building {
    pub(crate) fn of(gate: Arc<Gate>, held: Vec<IndexId>) -> Self {
        Self { gate, held }
    }

    /// This index now holds every row it should.
    pub(crate) fn filled(&mut self, index: IndexId) -> Result<(), DocumentError> {
        self.held.retain(|held| *held != index);
        self.gate.opened(index)
    }

    /// Filling the indexes still held was refused, for this reason.
    ///
    /// Takes every one of them rather than a single index, because a refusal
    /// stops the whole rebuild: an index it had not reached yet is as unfillable
    /// as the one that refused. The guard is emptied, so the [`Drop`] that
    /// follows has nothing left to call abandoned and the reason survives.
    pub(crate) fn failed(&mut self, error: &DocumentError) {
        for index in std::mem::take(&mut self.held) {
            self.gate.failed(index, error);
        }
    }
}

impl Drop for Building {
    fn drop(&mut self) {
        for index in &self.held {
            self.gate.abandoned(*index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static ONE: IndexId = IndexId("one");
    static TWO: IndexId = IndexId("two");

    #[tokio::test]
    async fn a_gate_over_nothing_lets_everything_through() {
        let gate = Gate::over(&[]);
        gate.clear(&[ONE, TWO]).await.expect("nothing is building");
    }

    /// The point of waiting per index rather than per handle: work that reads
    /// something else is not slowed down by a rebuild.
    #[tokio::test]
    async fn an_index_nobody_is_filling_does_not_park_a_read() {
        let gate = Gate::over(&[ONE]);
        gate.clear(&[TWO]).await.expect("two is not building");
    }

    #[tokio::test]
    async fn a_filled_index_stops_parking_reads() {
        let gate = Arc::new(Gate::over(&[ONE]));
        let mut building = Building::of(Arc::clone(&gate), vec![ONE]);
        building.filled(ONE).expect("one is filled");
        gate.clear(&[ONE]).await.expect("one is filled");
    }

    /// The whole reason the senders live in the gate and the guard owns their
    /// removal. A spawner that drops the rebuild has to produce an error, not
    /// a read that waits forever.
    #[tokio::test]
    async fn an_abandoned_rebuild_is_an_error_rather_than_a_wait() {
        let gate = Arc::new(Gate::over(&[ONE]));
        let parked = gate.waiting(&[ONE]).expect("one is building");

        drop(Building::of(Arc::clone(&gate), vec![ONE]));

        for (index, answer) in parked {
            assert_eq!(index, ONE);
            assert!(
                answer.await.is_err(),
                "an abandoned rebuild cancels what it was going to wake"
            );
        }
        assert!(
            matches!(
                gate.clear(&[ONE]).await,
                Err(DocumentError::Rebuilding { index }) if index == ONE
            ),
            "a read arriving after the abandonment is told, not parked and not answered"
        );
    }

    /// A refusal is a reason, and the reader is owed it. Collapsing this into
    /// abandonment is what made a taken unique key report itself as a rebuild
    /// going away.
    #[tokio::test]
    async fn a_refused_rebuild_tells_a_parked_reader_why() {
        let gate = Arc::new(Gate::over(&[ONE]));
        let parked = gate.waiting(&[ONE]).expect("one is building");

        let mut building = Building::of(Arc::clone(&gate), vec![ONE]);
        building.failed(&DocumentError::Duplicate {
            index: "one_by_key",
        });
        drop(building);

        for (_index, answer) in parked {
            assert!(answer.await.is_err(), "the refusal woke the reader");
        }
        assert!(
            matches!(
                gate.clear(&[ONE]).await,
                Err(DocumentError::Duplicate {
                    index: "one_by_key"
                })
            ),
            "the reason survives the guard being dropped"
        );
    }

    /// The same answer for a read that was not there when it happened, so the
    /// two paths into the gate cannot disagree about one failure.
    #[tokio::test]
    async fn a_reader_arriving_after_a_refusal_gets_the_same_reason() {
        let gate = Arc::new(Gate::over(&[ONE]));
        let mut building = Building::of(Arc::clone(&gate), vec![ONE]);
        building.failed(&DocumentError::NotFound);
        drop(building);

        assert!(matches!(
            gate.clear(&[ONE]).await,
            Err(DocumentError::NotFound)
        ));
    }

    /// A refusal stops the whole rebuild, so an index it had not reached is as
    /// unfillable as the one that refused.
    #[tokio::test]
    async fn a_refusal_covers_every_index_the_rebuild_still_held() {
        let gate = Arc::new(Gate::over(&[ONE, TWO]));
        let mut building = Building::of(Arc::clone(&gate), vec![ONE, TWO]);
        building.failed(&DocumentError::NotFound);
        drop(building);

        assert!(matches!(
            gate.clear(&[TWO]).await,
            Err(DocumentError::NotFound)
        ));
    }

    /// The fast path. A gate over nothing is never armed, and one whose last
    /// index fills stops being armed, or every operation keeps paying for a
    /// rebuild that finished.
    #[test]
    fn a_gate_stops_being_armed_once_the_last_index_fills() {
        assert!(!Gate::over(&[]).armed(), "nothing to hold anything back");

        let gate = Arc::new(Gate::over(&[ONE, TWO]));
        assert!(gate.armed());

        let mut building = Building::of(Arc::clone(&gate), vec![ONE, TWO]);
        building.filled(ONE).expect("one is filled");
        assert!(gate.armed(), "two is still outstanding");

        building.filled(TWO).expect("two is filled");
        assert!(!gate.armed(), "nothing is outstanding");
    }

    /// A refused index still has rows missing, so the gate has to stay armed or
    /// a later read would skip the check and answer out of it.
    #[test]
    fn a_refused_index_keeps_the_gate_armed() {
        let gate = Arc::new(Gate::over(&[ONE]));
        let mut building = Building::of(Arc::clone(&gate), vec![ONE]);
        building.failed(&DocumentError::NotFound);
        drop(building);

        assert!(
            gate.armed(),
            "an index that could not be filled still gates"
        );
    }

    /// The same for one nothing is filling any more.
    #[test]
    fn an_abandoned_index_keeps_the_gate_armed() {
        let gate = Arc::new(Gate::over(&[ONE]));
        drop(Building::of(Arc::clone(&gate), vec![ONE]));

        assert!(gate.armed());
    }

    /// A rebuild that finished has nothing left for the guard to abandon, so
    /// dropping it must not strand a later reader.
    #[tokio::test]
    async fn a_finished_rebuild_leaves_nothing_to_abandon() {
        let gate = Arc::new(Gate::over(&[ONE, TWO]));
        let mut building = Building::of(Arc::clone(&gate), vec![ONE, TWO]);
        building.filled(ONE).expect("one is filled");
        building.filled(TWO).expect("two is filled");
        drop(building);

        gate.clear(&[ONE, TWO]).await.expect("both are filled");
    }
}
