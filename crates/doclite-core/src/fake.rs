#![cfg(test)]
//! An engine a test can script, so this crate's own decisions are testable.
//!
//! `Pass::swept` and `Reading::planned` are the two places core decides what to
//! offer an engine and what to do when it declines, and neither can be reached
//! without one. `doclite-store` answers the same problem with a hand-written
//! `Region`; this is that, one seam up.

use std::sync::Mutex;

use async_trait::async_trait;

use crate::engine::{Answers, Applied, Batch, Defined, Engine, Plan, Reads, Sweep};
use crate::model::index::{IndexId, IndexSchema};
use crate::model::row::Row;
use crate::outcome::error::StorageError;
use crate::query::value::ReadValue;

/// What an engine was offered, in the order it was offered.
#[derive(Default)]
pub(crate) struct Offered {
    pub(crate) sweeps: Vec<Sweep>,
    pub(crate) plans: Vec<Plan>,
    pub(crate) scans: usize,
    pub(crate) batches: usize,
}

/// An engine that answers what it was told to and remembers what it was asked.
pub(crate) struct Fake {
    rows: Vec<(IndexId, Row)>,
    takes_sweeps: bool,
    plans: Option<ReadValue>,
    offered: Mutex<Offered>,
}

impl Fake {
    /// Declines every hint, which is what every engine did before the pushdown
    /// existed and is the baseline the others are read against.
    pub(crate) fn declining() -> Self {
        Self {
            rows: Vec::new(),
            takes_sweeps: false,
            plans: None,
            offered: Mutex::new(Offered::default()),
        }
    }

    pub(crate) fn holding(mut self, index: IndexId, rows: Vec<Row>) -> Self {
        self.rows.extend(rows.into_iter().map(|row| (index, row)));
        self
    }

    pub(crate) fn taking_sweeps(mut self) -> Self {
        self.takes_sweeps = true;
        self
    }

    pub(crate) fn planning(mut self, answer: ReadValue) -> Self {
        self.plans = Some(answer);
        self
    }

    pub(crate) fn offered(&self) -> std::sync::MutexGuard<'_, Offered> {
        self.offered.lock().expect("no test poisons this")
    }
}

#[async_trait]
impl Engine for Fake {
    async fn define(&self, _indexes: &[IndexSchema]) -> Result<Defined, StorageError> {
        Ok(Defined::ready())
    }

    async fn read(&self, reads: &Reads<'_>) -> Result<Answers, StorageError> {
        self.offered().scans += reads.scans.len();
        let scans = reads
            .scans
            .iter()
            .map(|read| {
                self.rows
                    .iter()
                    .filter(|(index, row)| {
                        *index == read.index && read.range.contains(row.key.components())
                    })
                    .map(|(_, row)| row.clone())
                    .collect()
            })
            .collect();
        Ok(Answers {
            scans,
            objects: vec![None; reads.objects.len()],
        })
    }

    async fn apply(&self, batch: &Batch<'_>) -> Result<Applied, StorageError> {
        let mut offered = self.offered();
        offered.batches += 1;
        if let Batch::Sweep(sweep) = batch {
            offered.sweeps.push((*sweep).clone());
        }
        Ok(Applied::Committed)
    }

    async fn plan(&self, plan: &Plan) -> Result<Option<ReadValue>, StorageError> {
        self.offered().plans.push(plan.clone());
        Ok(self.plans.clone())
    }

    async fn takes_sweep(&self, _sweep: &Sweep) -> bool {
        self.takes_sweeps
    }
}
