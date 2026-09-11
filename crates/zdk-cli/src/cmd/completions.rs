//! `zdk completions <shell>` — generated on a 16 MiB thread: clap's tree walk is deep and the
//! Windows main thread has only 1 MiB (see `build.rs`).

use clap::CommandFactory;
use zdk_core::output::write_stdout;
use zdk_core::{Result, ZdkError};

pub const GENERATOR_STACK_BYTES: usize = 16 * 1024 * 1024;

pub fn run(shell: clap_complete::Shell) -> Result<()> {
    let script = std::thread::Builder::new()
        .name("zdk-completions".into())
        .stack_size(GENERATOR_STACK_BYTES)
        .spawn(move || {
            let mut cmd = crate::cli::Cli::command();
            let mut out = Vec::new();
            clap_complete::generate(shell, &mut cmd, "zdk", &mut out);
            out
        })
        .map_err(|e| ZdkError::Other(format!("cannot start the completion generator: {e}")))?
        .join()
        .map_err(|_| ZdkError::Other("completion generator panicked".into()))?;
    write_stdout(&script);
    Ok(())
}
