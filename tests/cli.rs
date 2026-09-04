use std::process::Command;

#[test]
fn help_works_without_configuration_and_omits_legacy_options() {
    let home = temp_home("help");
    std::fs::create_dir_all(&home).unwrap();

    let output = glint(&home).arg("--help").output().unwrap();

    assert!(output.status.success(), "{}", stderr(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: glint"));
    assert!(!stdout.contains("init"));
    assert!(!stdout.contains("--config"));
}

#[test]
fn version_works_without_configuration() {
    let home = temp_home("version");
    std::fs::create_dir_all(&home).unwrap();

    let output = glint(&home).arg("--version").output().unwrap();

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "glint 0.1.0"
    );
}

#[test]
fn legacy_config_option_is_rejected() {
    let home = temp_home("config-option");
    std::fs::create_dir_all(&home).unwrap();

    let output = glint(&home)
        .args(["--config", "legacy.yaml"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(stderr(&output).contains("unexpected argument '--config'"));
}

#[test]
fn legacy_init_subcommand_is_rejected() {
    let home = temp_home("init");
    std::fs::create_dir_all(&home).unwrap();

    let output = glint(&home).arg("init").output().unwrap();

    assert!(!output.status.success());
    assert!(stderr(&output).contains("unexpected argument 'init'"));
}

#[test]
fn unconfigured_run_explains_interactive_setup() {
    let home = temp_home("unconfigured");
    let workspace = home.join("workspace");
    std::fs::create_dir_all(workspace.join(".glint")).unwrap();
    std::fs::write(workspace.join("config.yaml"), "legacy config").unwrap();
    std::fs::write(workspace.join(".glint/config.yaml"), "project config").unwrap();

    let output = glint(&home)
        .current_dir(&workspace)
        .env("GLINT_CONFIG", "/ignored/legacy.yaml")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert!(
        stderr
            .contains("no model is configured; run `glint` in an interactive terminal to add one")
    );
    assert!(!stderr.contains("ignored/legacy.yaml"));
    assert!(!stderr.contains(&workspace.join("config.yaml").display().to_string()));
    assert!(!stderr.contains(&workspace.join(".glint/config.yaml").display().to_string()));
}

fn glint(home: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_glint"));
    command
        .current_dir(std::env::temp_dir())
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME");
    command
}

fn temp_home(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("glint-cli-{label}-{}", uuid::Uuid::new_v4()))
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
