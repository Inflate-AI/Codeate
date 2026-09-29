use std::process::Command;

fn codeate() -> Command {
    Command::new(env!("CARGO_BIN_EXE_codeate"))
}

#[test]
fn version_flag_exits_zero_and_prints_the_binary_name_and_version() {
    let output = codeate()
        .arg("--version")
        .output()
        .expect("run codeate --version");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.contains("codeate"));
    assert!(stdout.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_flag_exits_zero_and_prints_usage() {
    let output = codeate()
        .arg("--help")
        .output()
        .expect("run codeate --help");

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("utf8 stdout");
    assert!(stdout.to_lowercase().contains("usage"));
}

#[test]
fn no_args_exits_zero() {
    let output = codeate().output().expect("run codeate with no args");

    assert!(output.status.success());
}
