//! What holds a read back while the index it names is still being filled.
//!
//! It lives here rather than on `Db` because only this layer can ever open it.
//! Filling an index means deriving rows from objects, and an object is bytes to
//! everything below [`Record`](crate::Record), so a gate in `doclite-core`
//! could park a byte-level caller with nothing in the world able to release it.
//! `Db::building` reports; this waits.
//!
//! The waiters are `futures_channel::oneshot` receivers, which belong to no
//! runtime (ADR 0006). The senders are held *here* and removed by [`Building`]
//! when it drops, which is what turns an abandoned rebuild into an error rather
//! than a hang: a spawner that drops the future drops the guard, the guard
//! drops the senders, and every parked read wakes with
//! [`DocumentError::Rebuilding`].
//!
//! Nothing holds the lock across an await. [`Gate::waiting`] takes it, collects
//! a receiver per index, and gives it up before any of them is awaited.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use doclite_core::{IndexId, StorageError};
use futures_channel::oneshot;

use crate::error::DocumentError;

type Waiters = BTreeMap<IndexId, Filling>;

/// One index and the channel a read parked on it is waiting on.
type Parked = (IndexId, oneshot::Receiver<()>);

/// Where an index that is not yet readable has got to.
///
/// Absence from the map is the third state and means filled. Keeping
/// abandonment as its own entry rather than a removal is what stops a read from
/// mistaking "nothing will ever fill this" for "this is complete" and answering
/// out of an index with rows missing.
enum Filling {
    /// Something is filling it, and these are waiting for it to finish.
    Running(Vec<oneshot::Sender<()>>),
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
}

impl Gate {
    /// A gate over the indexes an engine said it has rows missing from.
    pub(crate) fn over(building: &[IndexId]) -> Self {
        Self {
            building: Mutex::new(
                building
                    .iter()
                    .map(|index| (*index, Filling::Running(Vec::new())))
                    .collect(),
            ),
        }
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
                Err(_abandoned) => return Err(DocumentError::Rebuilding { index }),
            }
        }
        Ok(())
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
        let waiters = match self.held()?.remove(&index) {
            None | Some(Filling::Abandoned) => Vec::new(),
            Some(Filling::Running(waiters)) => waiters,
        };
        for waiter in waiters {
            match waiter.send(()) {
                Ok(()) => {}
                Err(_gone) => {}
            }
        }
        Ok(())
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
/// Its only job is [`Drop`]. A rebuild that finishes calls [`Building::filled`]
/// per index and the guard has nothing left to abandon; one that is dropped
/// part way, because the spawner discarded the future or the task was
/// cancelled, releases every index it still holds *without* firing, and the
/// reads parked on them get an error naming the index instead of waiting for
/// something that will never arrive.
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
