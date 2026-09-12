use std::ffi::OsStr;

use portable_pty::CommandBuilder;

pub(crate) const ID: &str = "herdl";
pub(crate) const DISPLAY_NAME: &str = "HerDL";
pub(crate) const BINARY_NAME: &str = "herdl";
pub(crate) const APP_DIR_NAME: &str = "herdl";
pub(crate) const DEBUG_APP_DIR_NAME: &str = "herdl-dev";
pub(crate) const HOME_URL: &str = "https://github.com/victor-software-house/herdr";

pub(crate) const ENV_VAR: &str = "HERDL_ENV";
pub(crate) const ENV_VALUE: &str = "1";
pub(crate) const CONFIG_PATH_ENV_VAR: &str = "HERDL_CONFIG_PATH";
pub(crate) const SESSION_ENV_VAR: &str = "HERDL_SESSION";
pub(crate) const SOCKET_PATH_ENV_VAR: &str = "HERDL_SOCKET_PATH";
pub(crate) const CLIENT_SOCKET_PATH_ENV_VAR: &str = "HERDL_CLIENT_SOCKET_PATH";
pub(crate) const LOG_ENV_VAR: &str = "HERDL_LOG";
pub(crate) const BIN_PATH_ENV_VAR: &str = "HERDL_BIN_PATH";
pub(crate) const STARTUP_CWD_ENV_VAR: &str = "HERDL_STARTUP_CWD";
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) const AGENT_ENV_RECORD_PREFIX: &[u8] = b"HERDL_AGENT=";
#[cfg(target_os = "linux")]
pub(crate) const PROCESS_DETECTION_ENV_VAR: &str = "HERDL_PROCESS_DETECTION";
#[cfg(windows)]
pub(crate) const PANE_RUNTIME_ID_ENV_VAR: &str = "HERDL_PANE_RUNTIME_ID";
pub(crate) const WORKSPACE_ID_ENV_VAR: &str = "HERDL_WORKSPACE_ID";
pub(crate) const TAB_ID_ENV_VAR: &str = "HERDL_TAB_ID";
pub(crate) const PANE_ID_ENV_VAR: &str = "HERDL_PANE_ID";
pub(crate) const ACTIVE_WORKSPACE_ID_ENV_VAR: &str = "HERDL_ACTIVE_WORKSPACE_ID";
pub(crate) const ACTIVE_TAB_ID_ENV_VAR: &str = "HERDL_ACTIVE_TAB_ID";
pub(crate) const ACTIVE_PANE_ID_ENV_VAR: &str = "HERDL_ACTIVE_PANE_ID";
pub(crate) const ACTIVE_PANE_CWD_ENV_VAR: &str = "HERDL_ACTIVE_PANE_CWD";
pub(crate) const PLUGIN_ID_ENV_VAR: &str = "HERDL_PLUGIN_ID";
pub(crate) const PLUGIN_ROOT_ENV_VAR: &str = "HERDL_PLUGIN_ROOT";
pub(crate) const PLUGIN_CONFIG_DIR_ENV_VAR: &str = "HERDL_PLUGIN_CONFIG_DIR";
pub(crate) const PLUGIN_STATE_DIR_ENV_VAR: &str = "HERDL_PLUGIN_STATE_DIR";
pub(crate) const PLUGIN_ENTRYPOINT_ID_ENV_VAR: &str = "HERDL_PLUGIN_ENTRYPOINT_ID";
pub(crate) const PLUGIN_CONTEXT_JSON_ENV_VAR: &str = "HERDL_PLUGIN_CONTEXT_JSON";
pub(crate) const PLUGIN_ACTION_ID_ENV_VAR: &str = "HERDL_PLUGIN_ACTION_ID";
pub(crate) const PLUGIN_EVENT_ENV_VAR: &str = "HERDL_PLUGIN_EVENT";
pub(crate) const PLUGIN_EVENT_JSON_ENV_VAR: &str = "HERDL_PLUGIN_EVENT_JSON";
pub(crate) const PLUGIN_CLICKED_URL_ENV_VAR: &str = "HERDL_PLUGIN_CLICKED_URL";
pub(crate) const PLUGIN_LINK_HANDLER_ID_ENV_VAR: &str = "HERDL_PLUGIN_LINK_HANDLER_ID";
pub(crate) const API_SOCKET_FILE: &str = "herdl.sock";
pub(crate) const CLIENT_SOCKET_FILE: &str = "herdl-client.sock";
pub(crate) const LOG_FILE: &str = "herdl.log";
pub(crate) const CLIENT_LOG_FILE: &str = "herdl-client.log";
pub(crate) const SERVER_LOG_FILE: &str = "herdl-server.log";
#[cfg(unix)]
pub(crate) const PANE_GRAPHICS_PREFIX: &str = "herdl-pane-graphics";

const FOREIGN_ENV_PREFIX: &str = "HERDR_";

pub(crate) fn app_dir_name() -> &'static str {
    if cfg!(debug_assertions) {
        DEBUG_APP_DIR_NAME
    } else {
        APP_DIR_NAME
    }
}

pub(crate) fn scrub_foreign_process_env() {
    for (key, _) in std::env::vars_os() {
        if is_foreign_env_key(&key) {
            std::env::remove_var(key);
        }
    }
}

pub(crate) fn scrub_foreign_command_env(command: &mut std::process::Command) {
    for (key, _) in std::env::vars_os() {
        if is_foreign_env_key(&key) {
            command.env_remove(key);
        }
    }
}

pub(crate) fn scrub_foreign_pty_env(command: &mut CommandBuilder) {
    for (key, _) in std::env::vars_os() {
        if is_foreign_env_key(&key) {
            command.env_remove(key);
        }
    }
}

pub(crate) fn is_foreign_env_key(key: &OsStr) -> bool {
    key.to_string_lossy().starts_with(FOREIGN_ENV_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    #[test]
    fn product_identity_is_independent_from_official_herdr() {
        assert_eq!(ID, "herdl");
        assert_eq!(BINARY_NAME, "herdl");
        assert_eq!(CONFIG_PATH_ENV_VAR, "HERDL_CONFIG_PATH");
        assert_eq!(SOCKET_PATH_ENV_VAR, "HERDL_SOCKET_PATH");
        assert_ne!(APP_DIR_NAME, "herdr");
        assert_ne!(API_SOCKET_FILE, "herdr.sock");
    }

    #[test]
    fn child_commands_remove_inherited_official_routing() {
        let _guard = env_lock();
        std::env::set_var("HERDR_SOCKET_PATH", "/tmp/official.sock");
        std::env::set_var("HERDL_SOCKET_PATH", "/tmp/downstream.sock");

        let mut command = std::process::Command::new("true");
        scrub_foreign_command_env(&mut command);
        let env = command
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(ToOwned::to_owned)))
            .collect::<Vec<_>>();

        std::env::remove_var("HERDR_SOCKET_PATH");
        std::env::remove_var("HERDL_SOCKET_PATH");

        assert!(env
            .iter()
            .any(|(key, value)| { key == "HERDR_SOCKET_PATH" && value.is_none() }));
        assert!(!env.iter().any(|(key, _)| key == "HERDL_SOCKET_PATH"));
    }

    #[test]
    fn entry_scrub_removes_only_official_routing() {
        let _guard = env_lock();
        std::env::set_var("HERDR_SESSION", "official");
        std::env::set_var(SESSION_ENV_VAR, "downstream");

        scrub_foreign_process_env();

        assert!(std::env::var_os("HERDR_SESSION").is_none());
        assert_eq!(std::env::var(SESSION_ENV_VAR).as_deref(), Ok("downstream"));
        std::env::remove_var(SESSION_ENV_VAR);
    }

    #[test]
    fn config_and_state_roots_are_disjoint_from_official_herdr() {
        let _guard = env_lock();
        let root = std::env::temp_dir().join(format!("herdl-product-roots-{}", std::process::id()));
        std::env::set_var("XDG_CONFIG_HOME", root.join("config"));
        std::env::set_var("XDG_STATE_HOME", root.join("state"));

        assert_eq!(
            crate::config::config_dir(),
            root.join("config").join(app_dir_name())
        );
        assert_eq!(
            crate::config::state_dir(),
            root.join("state").join(app_dir_name())
        );
        assert_ne!(crate::config::config_dir(), root.join("config/herdr-dev"));
        assert_ne!(crate::config::state_dir(), root.join("state/herdr-dev"));

        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::remove_var("XDG_STATE_HOME");
        let _ = std::fs::remove_dir_all(root);
    }
}
