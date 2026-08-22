//! Build-time CLI identity.
//!
//! Protocol, sockets, config dir, and `HERDR_*` pane environment stay Herdr's so
//! this binary can attach to a live server. argv0, clap, and usage strings come
//! from [`APP_NAME`].
//!
//! Override at compile time:
//!
//! ```sh
//! HDR_APP_NAME=hdr cargo build --release
//! ```

/// CLI argv0 / clap bin name. Set by `build.rs` from `HDR_APP_NAME` (default `hdr`).
pub const APP_NAME: &str = env!("HDR_APP_NAME");

/// `usage: {APP_NAME} {spec}`
pub fn usage(spec: &str) -> String {
    format!("usage: {APP_NAME} {spec}")
}

/// `{APP_NAME}` or `{APP_NAME} {spec}` for help tables and continuations.
pub fn command(spec: &str) -> String {
    if spec.is_empty() {
        APP_NAME.to_string()
    } else {
        format!("{APP_NAME} {spec}")
    }
}

pub fn agent_help_footer() -> String {
    format!(
        "Are you an AI? This binary is `{APP_NAME}`.\n\
         Do not install it over `herdr` on PATH.\n\
         Attach: {APP_NAME} terminal attach <terminal_id>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_is_format_over_app_name() {
        assert_eq!(
            usage("pane get <pane_id>"),
            format!("usage: {APP_NAME} pane get <pane_id>")
        );
    }

    #[test]
    fn command_is_format_over_app_name() {
        assert_eq!(command("pane list"), format!("{APP_NAME} pane list"));
        assert_eq!(command(""), APP_NAME);
    }

    #[test]
    fn app_name_is_a_single_token() {
        assert!(!APP_NAME.is_empty());
        assert!(!APP_NAME.contains(char::is_whitespace));
        assert!(!APP_NAME.contains('/'));
    }
}
