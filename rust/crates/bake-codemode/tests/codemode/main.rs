//! `bake-codemode`'s integration tests, one binary so every case shares one
//! compiled engine: the engine's own cases (`engine`), Pi v1.3.0's
//! `packages/codemode/test/sandbox.test.ts` (`sandbox`),
//! `deterministic.test.ts` (`deterministic`), and `worker-lifetime.test.ts`
//! (`lifetime`), each naming its Pi case.

mod deterministic;
mod engine;
mod harness;
mod lifetime;
mod sandbox;
