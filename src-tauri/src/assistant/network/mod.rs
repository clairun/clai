//! Network controls shared by the sandboxed shell and the web tools.

pub mod addr;
// `allow`, not `expect`: dead-code expectations on modules that use each
// other are not reliably fulfilled. Both go when bash_exec starts sessions.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "bash_exec starts proxy sessions in a later commit")
)]
pub mod connect;
pub mod default_lists;
pub mod http;
pub mod matcher;
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "bash_exec starts proxy sessions in a later commit")
)]
pub mod proxy;
pub mod run_state;
