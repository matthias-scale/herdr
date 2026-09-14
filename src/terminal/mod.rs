mod history_read;
mod id;
mod metadata;
mod runtime;
mod runtime_registry;
pub mod state;
mod title;

pub(crate) use history_read::{merge_scrolled_up, snapshot_text, ScreenSnapshot, UpwardMerge};
pub use id::TerminalId;
pub use metadata::{AgentMetadata, AgentMetadataReport, EffectivePresentation};
pub use runtime::TerminalRuntime;
pub(crate) use runtime_registry::TerminalRuntimeRegistry;
#[cfg(unix)]
pub(crate) use state::{AgentActivityHandoffState, TerminalAgentHandoffState};
pub use state::{EffectiveStateChange, TerminalState, TerminalStateMutation};
pub(crate) use title::stripped_terminal_title;
