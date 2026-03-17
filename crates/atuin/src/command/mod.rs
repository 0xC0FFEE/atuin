use clap::Subcommand;
use eyre::Result;

#[cfg(not(windows))]
use rustix::{fs::Mode, process::umask};

#[cfg(feature = "client")]
mod client;

mod gen_completions;

#[derive(Subcommand)]
#[command(infer_subcommands = true)]
#[allow(clippy::large_enum_variant)]
pub enum AtuinCmd {
    #[cfg(feature = "client")]
    #[command(flatten)]
    Client(client::Cmd),

    /// Generate shell completions
    GenCompletions(gen_completions::Cmd),
}

impl AtuinCmd {
    pub fn run(self) -> Result<()> {
        #[cfg(not(windows))]
        {
            // set umask before we potentially open/create files
            // or in other words, 077. Do not allow any access to any other user
            let mode = Mode::RWXG | Mode::RWXO;
            umask(mode);
        }

        match self {
            #[cfg(feature = "client")]
            Self::Client(client) => client.run(),

            Self::GenCompletions(gen_completions) => gen_completions.run(),
        }
    }
}
