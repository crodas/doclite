//! The memory engine against the definition of a correct engine.
//!
//! Nothing here is specific to this engine, which is the point: a second
//! backend inherits every rule by saying how to build one of its own.

use async_trait::async_trait;
use doclite_core::StorageError;
use doclite_memory::MemoryEngine;
use doclite_testkit::Fresh;

/// What the suite builds engines through.
struct Memory;

#[async_trait]
impl Fresh for Memory {
    type Engine = MemoryEngine;

    async fn fresh() -> Result<MemoryEngine, StorageError> {
        Ok(MemoryEngine::new())
    }
}

doclite_testkit::conformance!(
    #[tokio::test]
    Memory
);
