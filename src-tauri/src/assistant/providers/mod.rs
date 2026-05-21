pub mod anthropic;
pub mod cli;
pub mod openai;
pub mod registry;
pub mod types;

pub use registry::{
    get_provider_descriptor, is_cli_provider, resolve_adapter, supported_providers,
};
