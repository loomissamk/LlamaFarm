#[allow(clippy::module_inception)]
pub mod agent;
pub mod autonomous;
pub mod classifier;
pub mod dispatcher;
pub mod loop_;
pub mod memory_loader;
pub mod prompt;
pub mod repo_workflow;
pub mod research;
pub mod tool_cache;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use agent::{Agent, AgentBuilder};
#[allow(unused_imports)]
pub use autonomous::{AutonomousLoop, LoopOutcome};
#[allow(unused_imports)]
pub use loop_::{process_message, run};
#[allow(unused_imports)]
pub use tool_cache::ToolResultCache;
