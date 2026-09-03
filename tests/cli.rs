use std::process::Command;

#[test]
fn help_works_without_configuration() {
    let cwd = temp_dir("help");
    std::fs::create_dir_all(&cwd).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_glint"))
        .arg("--help")
        .current_dir(cwd)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", stderr(&output));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: glint"));
    assert!(stdout.contains("init"));
    assert!(stdout.contains("--config"));
}

#[test]
fn version_works_without_configuration() {
    let cwd = temp_dir("version");
    std::fs::create_dir_all(&cwd).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_glint"))
        .arg("--version")
        .current_dir(cwd)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "glint 0.1.0"
    );
}

#[test]
fn init_writes_the_starter_config_to_an_explicit_path() {
    let cwd = temp_dir("init");
    let config_path = cwd.join("nested/config.yaml");
    std::fs::create_dir_all(&cwd).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_glint"))
        .args(["init", "--config"])
        .arg(&config_path)
        .current_dir(&cwd)
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        std::fs::read_to_string(&config_path).unwrap(),
        include_str!("../config.example.yaml")
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(&config_path.display().to_string()));
}

#[test]
fn init_refuses_to_overwrite_an_existing_config() {
    let cwd = temp_dir("init-existing");
    let config_path = cwd.join("config.yaml");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::write(&config_path, "keep me\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_glint"))
        .args(["init", "--config"])
        .arg(&config_path)
        .current_dir(&cwd)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert_eq!(std::fs::read_to_string(&config_path).unwrap(), "keep me\n");
    assert!(stderr(&output).contains("already exists"));
}

#[test]
fn init_uses_xdg_config_home_when_no_path_is_given() {
    let cwd = temp_dir("init-xdg");
    let xdg = cwd.join("xdg");
    let expected = xdg.join("glint/config.yaml");
    std::fs::create_dir_all(&cwd).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_glint"))
        .arg("init")
        .current_dir(&cwd)
        .env_remove("GLINT_CONFIG")
        .env("XDG_CONFIG_HOME", &xdg)
        .env_remove("HOME")
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        std::fs::read_to_string(&expected).unwrap(),
        include_str!("../config.example.yaml")
    );
}

#[test]
fn default_run_without_configuration_reports_attempts_and_init_hint() {
    let cwd = temp_dir("missing-config");
    let home = cwd.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_glint"))
        .current_dir(&cwd)
        .env_remove("GLINT_CONFIG")
        .env_remove("XDG_CONFIG_HOME")
        .env("HOME", &home)
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = stderr(&output);
    assert!(stderr.contains(&cwd.join(".glint/config.yaml").display().to_string()));
    assert!(stderr.contains("glint init"));
}

fn temp_dir(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("glint-cli-{label}-{}", uuid::Uuid::new_v4()))
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
