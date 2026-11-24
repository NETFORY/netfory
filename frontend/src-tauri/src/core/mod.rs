// Author: TechnoL0g
// Project: SmartNet Core
//
// Core protocol primitives shared across the SmartNet node.

// Not all proof-of-storage entry points are wired into the live network path yet
// (challenge dispatch lands with Phase F); keep them compiled + unit-tested.
#[allow(dead_code)]
pub mod storage_proof;
