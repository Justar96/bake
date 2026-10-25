# Coding-agent fixtures

Cargo tests read the recorded JSON, JSONL, text, and image fixtures directly. Their provenance fields and the consuming Rust tests name the upstream revision and behavior each record covers. Preserve these inputs and upstream attribution when adapting ported behavior; replacing them with output from the implementation under test would erase the oracle.

The original recording programs can be inspected in [the source revision](https://github.com/Justar96/bake/tree/97ce6abba01fddb5ed532b60a2e25a1e134b5244/rust/crates/bake-coding-agent/tests/fixtures). They recorded Pi session reads and writes, compaction, file tools, bash, execution, output, mutation queues, skills, and MCP names. They are not required to run the Rust tests.
