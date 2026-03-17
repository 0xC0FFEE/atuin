use atuin_client::settings::Tmux;

pub fn init_static(disable_up_arrow: bool, disable_ctrl_r: bool, _tmux: &Tmux) {
    let base = include_str!("../../../shell/atuin.xsh");

    let (bind_ctrl_r, bind_up_arrow) = if std::env::var("ATUIN_NOBIND").is_ok() {
        (false, false)
    } else {
        (!disable_ctrl_r, !disable_up_arrow)
    };

    // TODO: tmux popup for xonsh
    println!(
        "_ATUIN_BIND_CTRL_R={}",
        if bind_ctrl_r { "True" } else { "False" }
    );
    println!(
        "_ATUIN_BIND_UP_ARROW={}",
        if bind_up_arrow { "True" } else { "False" }
    );
    println!("{base}");
}
