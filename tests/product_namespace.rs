use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn unique_temp_dir() -> PathBuf {
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("herdl-product-test-{}-{id}", std::process::id()))
}

#[test]
fn version_and_help_use_only_herdl_product_identity() {
    let version = Command::new(env!("CARGO_BIN_EXE_herdl"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("herdl "));

    let help = Command::new(env!("CARGO_BIN_EXE_herdl"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(help.status.success());
    let stdout = String::from_utf8_lossy(&help.stdout);
    assert!(stdout.contains("HerDL panes, agents, or workspaces"));
    assert!(stdout.contains("herdl --skill"));
    assert!(
        !stdout.contains("herdr.dev"),
        "official docs leaked: {stdout}"
    );
    assert!(
        !stdout.contains("herdr --skill"),
        "official command leaked: {stdout}"
    );
}

#[test]
fn config_check_ignores_official_routing_and_honors_herdl_routing() {
    let base = unique_temp_dir();
    std::fs::create_dir_all(&base).unwrap();
    let official_config = base.join("official-herdr.toml");
    let herdl_config = base.join("herdl.toml");
    std::fs::write(&official_config, "not valid toml = [").unwrap();
    std::fs::write(&herdl_config, "not valid toml = [").unwrap();

    let ignores_official = Command::new(env!("CARGO_BIN_EXE_herdl"))
        .args(["config", "check"])
        .env("HERDR_CONFIG_PATH", &official_config)
        .env("HERDR_SOCKET_PATH", base.join("official.sock"))
        .env("HERDL_CONFIG_PATH", base.join("missing-herdl.toml"))
        .env_remove("HERDL_SOCKET_PATH")
        .output()
        .unwrap();
    assert!(
        ignores_official.status.success(),
        "official routing affected HerDL: {}",
        String::from_utf8_lossy(&ignores_official.stderr)
    );

    let honors_herdl = Command::new(env!("CARGO_BIN_EXE_herdl"))
        .args(["config", "check"])
        .env("HERDR_CONFIG_PATH", &official_config)
        .env("HERDL_CONFIG_PATH", &herdl_config)
        .output()
        .unwrap();
    assert_eq!(honors_herdl.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&honors_herdl.stdout).contains("config: issues found"));

    let _ = std::fs::remove_dir_all(base);
}
