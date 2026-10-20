// `perseids plan` must print exactly what `ruby lib/runner.rb plan` prints, exit code and error included.
use std::path::Path;
use std::process::Command;

fn plan(cmd: &mut Command, args: &[&str]) -> (Option<i32>, String, String) {
    let o = cmd
        .args(args)
        .env("PERSEIDS_SHA", "0123456789abcdef")
        .env("PERSEIDS_REF_NAME", "main")
        .env("PERSEIDS_EVENT", "push")
        .output()
        .expect("runs");
    (o.status.code(), String::from_utf8_lossy(&o.stdout).into(), String::from_utf8_lossy(&o.stderr).into())
}

#[test]
fn plan_matches_runner_rb() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let runner = root.join("lib/runner.rb");
    let fx = |f: &str| Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(f).display().to_string();
    let ci = root.join(".github/workflows/ci.yml").display().to_string();
    let cases: Vec<Vec<String>> = vec![
        vec![ci.clone()],
        vec![ci.clone(), "unit".into(), "after".into()],
        vec![ci, "ghost".into()],
        vec![fx("matrix.yml")],
        vec![fx("waves.yml")],
        vec![fx("waves.yml"), "deploy".into(), "lint".into()],
        vec![fx("loop.yml")],
        vec![fx("missing.yml")],
        vec![fx("missing.yml"), "a".into()],
    ];
    for c in cases {
        let args: Vec<&str> = std::iter::once("plan").chain(c.iter().map(String::as_str)).collect();
        let ruby = plan(Command::new("ruby").arg(&runner).current_dir(root), &args);
        let rust = plan(Command::new(env!("CARGO_BIN_EXE_perseids")).current_dir(root), &args);
        assert_eq!(rust, ruby, "perseids {}", args.join(" "));
    }
}
