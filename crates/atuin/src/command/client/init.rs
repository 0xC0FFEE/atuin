use atuin_client::settings::{Settings, Tmux};
use clap::{Parser, ValueEnum};

mod bash;
mod fish;
mod powershell;
mod xonsh;
mod zsh;

#[derive(Parser, Debug)]
pub struct Cmd {
    shell: Shell,

    /// Disable the binding of CTRL-R to atuin
    #[clap(long)]
    disable_ctrl_r: bool,

    /// Disable the binding of the Up Arrow key to atuin
    #[clap(long)]
    disable_up_arrow: bool,
}

#[derive(Clone, Copy, ValueEnum, Debug)]
#[value(rename_all = "lower")]
#[allow(clippy::enum_variant_names, clippy::doc_markdown)]
pub enum Shell {
    /// Zsh setup
    Zsh,
    /// Bash setup
    Bash,
    /// Fish setup
    Fish,
    /// Nu setup
    Nu,
    /// Xonsh setup
    Xonsh,
    /// PowerShell setup
    PowerShell,
}

impl Cmd {
    fn init_nu(&self, _tmux: &Tmux) {
        let full = include_str!("../../shell/atuin.nu");

        // TODO: tmux popup for Nu
        println!("{full}");

        if std::env::var("ATUIN_NOBIND").is_err() {
            const BIND_CTRL_R: &str = r"$env.config = (
    $env.config | upsert keybindings (
        $env.config.keybindings
        | append {
            name: atuin
            modifier: control
            keycode: char_r
            mode: [emacs, vi_normal, vi_insert]
            event: { send: executehostcommand cmd: (_atuin_search_cmd) }
        }
    )
)";
            const BIND_UP_ARROW: &str = r"
$env.config = (
    $env.config | upsert keybindings (
        $env.config.keybindings
        | append {
            name: atuin
            modifier: none
            keycode: up
            mode: [emacs, vi_normal, vi_insert]
            event: {
                until: [
                    {send: menuup}
                    {send: executehostcommand cmd: (_atuin_search_cmd '--shell-up-key-binding') }
                ]
            }
        }
    )
)
";
            if !self.disable_ctrl_r {
                println!("{BIND_CTRL_R}");
            }
            if !self.disable_up_arrow {
                println!("{BIND_UP_ARROW}");
            }
        }
    }

    fn static_init(&self, tmux: &Tmux) {
        match self.shell {
            Shell::Zsh => {
                zsh::init_static(self.disable_up_arrow, self.disable_ctrl_r, tmux);
            }
            Shell::Bash => {
                bash::init_static(self.disable_up_arrow, self.disable_ctrl_r, tmux);
            }
            Shell::Fish => {
                fish::init_static(self.disable_up_arrow, self.disable_ctrl_r, tmux);
            }
            Shell::Nu => {
                self.init_nu(tmux);
            }
            Shell::Xonsh => {
                xonsh::init_static(self.disable_up_arrow, self.disable_ctrl_r, tmux);
            }
            Shell::PowerShell => {
                powershell::init_static(self.disable_up_arrow, self.disable_ctrl_r, tmux);
            }
        }
    }

    pub fn run(self, settings: &Settings) {
        if !settings.paths_ok() {
            eprintln!(
                "Atuin settings paths are broken. Disabling atuin shell hooks. Run `atuin doctor` to diagnose."
            );
            return;
        }

        self.static_init(&settings.tmux);
    }
}
