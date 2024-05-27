use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Write},
    process::{Command, Stdio},
};

mod ast;

fn main() {
    let out_path = "crates/httpwg-macros/src/lib.rs";
    if std::fs::symlink_metadata(out_path).is_err() {
        eprintln!("⛔️ Output path doesn't exist: {out_path}");
        eprintln!("(This tool expect to overwrite it, so the fact that it doesn't");
        eprintln!("already exist means you're probably running it from the wrong");
        eprintln!("directory.)");
        eprintln!("👉 This tool should only be run from the top-level of the fluke workspace.");
        panic!("Refusing to proceed, read stderr above");
    }

    println!("🧱 Generating rustdoc...");

    let mut cmd = Command::new("cargo");
    cmd.arg("rustdoc");
    cmd.args(["-Z", "unstable-options"]);
    cmd.args(["--output-format", "json"]);
    cmd.args(["--package", "httpwg"]);
    cmd.args(["--target-dir", "target-codegen"]);
    cmd.arg("--locked");
    cmd.env("RUSTC_BOOTSTRAP", "1");
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let final_cmd = format!("{cmd:?}");
    let mut child = cmd
        .spawn()
        .unwrap_or_else(|err| panic!("{err} while spawning command: {final_cmd}"));

    enum StdoutOrStderr {
        Stdout,
        Stderr,
    }

    fn collect_output(kind: StdoutOrStderr, r: impl Read) -> Vec<String> {
        let r = BufReader::new(r);
        let mut lines = Vec::new();
        for l in r.lines() {
            let l = l.unwrap();
            match kind {
                StdoutOrStderr::Stdout => println!("{l}"),
                StdoutOrStderr::Stderr => eprintln!("{l}"),
            }
            lines.push(l)
        }
        lines
    }

    let stdout = std::thread::spawn({
        let r = child.stdout.take().unwrap();
        move || collect_output(StdoutOrStderr::Stdout, r)
    });
    let stderr = std::thread::spawn({
        let r = child.stderr.take().unwrap();
        move || collect_output(StdoutOrStderr::Stderr, r)
    });

    let status = child
        .wait()
        .unwrap_or_else(|err| panic!("{err} while waiting for command: {final_cmd}"));
    if !status.success() {
        eprintln!("command failed: {final_cmd}");
        eprintln!("=== stdout");
        let stdout = stdout.join().unwrap();
        for line in stdout {
            eprintln!("{line}");
        }
        eprintln!("=== stderr");
        let stderr = stderr.join().unwrap();
        for line in stderr {
            eprintln!("{line}");
        }
        eprintln!("===");
        panic!("command returned status {status:?}, command was: {final_cmd}")
    }

    println!("🕵️‍♂️ Parsing type info");
    let json_path = "target-codegen/doc/httpwg.json";
    let json_payload = std::fs::read(json_path).unwrap();
    let doc: ast::Document = serde_json::from_slice(&json_payload).unwrap();
    assert!(
        doc.format_version >= 28,
        "This tool expects JSON format version 28",
    );
    println!("📝 Listing tests...");

    let root = doc.index.get(&doc.root).expect("Could not find root node");
    let module = match &root.inner {
        ast::ItemInner::Module(m) => m,
        _ => panic!("Root has to be module"),
    };

    #[derive(Debug)]
    struct Suite {
        name: String,
        docs: Option<String>,
        groups: Vec<Group>,
    }

    #[derive(Debug)]
    struct Group {
        name: String,
        docs: Option<String>,
        tests: Vec<Test>,
    }

    #[derive(Debug)]
    struct Test {
        name: String,
        docs: Option<String>,
    }

    let mut suites: Vec<Suite> = Default::default();

    for item_id in &module.items {
        let item = doc.index.get(item_id).expect("Could not find some node");
        match &item.inner {
            ast::ItemInner::Module(module) => {
                let suite_name = item.name.clone().unwrap();
                if suite_name.starts_with("rfc") {
                    // good!
                } else {
                    // skip
                    continue;
                }
                println!("📚 {suite_name} ({item_id})");
                let mut suite = Suite {
                    name: suite_name,
                    docs: item.docs.clone(),
                    groups: Default::default(),
                };

                for item_id in &module.items {
                    let item = doc.index.get(item_id).expect("Could not find some node");
                    match &item.inner {
                        ast::ItemInner::Module(module) => {
                            let group_name = item.name.clone().unwrap();
                            if group_name.starts_with('_') {
                                // good!
                            } else {
                                // skip
                                continue;
                            }
                            println!("  📕 {group_name} ({item_id})");

                            let mut group = Group {
                                name: group_name,
                                docs: item.docs.clone(),
                                tests: Default::default(),
                            };

                            for item_id in &module.items {
                                let item =
                                    doc.index.get(item_id).expect("Could not find some node");
                                match &item.inner {
                                    ast::ItemInner::Function(_) => {
                                        let test_name = item.name.clone().unwrap();
                                        println!("    📄 {test_name} ({item_id})");

                                        let test = Test {
                                            name: test_name,
                                            docs: item.docs.clone(),
                                        };
                                        group.tests.push(test);
                                    }
                                    _ => {
                                        // ignore
                                    }
                                }
                            }

                            suite.groups.push(group);
                        }
                        _ => {
                            // ignore
                        }
                    }
                }

                suites.push(suite);
            }
            _ => {
                // ignore
            }
        }
    }

    // Generate macro code, pipe it to rustfmt
    let mut cmd = Command::new("rustfmt");
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let final_cmd = format!("{cmd:?}");
    let mut child = cmd
        .spawn()
        .unwrap_or_else(|err| panic!("{err} while spawning command: {final_cmd}"));

    let stdout = std::thread::spawn({
        let r = child.stdout.take().unwrap();
        let mut f = File::create(out_path).unwrap();

        move || {
            let r = BufReader::new(r);
            for line in r.lines() {
                let line = line.unwrap();
                writeln!(&mut f, "{line}").unwrap();
            }
        }
    });

    let stderr = std::thread::spawn({
        let stderr = child.stderr.take().unwrap();
        move || collect_output(StdoutOrStderr::Stderr, stderr)
    });

    {
        let mut out = child.stdin.take().unwrap();

        macro_rules! w {
            ($tt:tt) => {
                writeln!(&mut out, $tt).unwrap()
            };
        }

        w!("//! Macros to help generate code for all suites/groups/tests of the httpwg crate");
        w!("");
        w!("// This file is automatically @generated by httpwg-gen");
        w!("// It is not intended for manual editing");
        w!("");
        w!("/// This generates a module tree with some #[test] functions.");
        w!("/// The `$body` argument is pasted inside those unit test, and");
        w!("/// in that scope, `test` is the `httpwg` function you can use");
        w!("/// to run the test (that takes a `mut conn: Conn<IO>`)");
        w!("#[macro_export]");
        w!("macro_rules! tests {{");
        {
            w!("  ($body: tt) => {{");
            for suite in &suites {
                let suite_name = &suite.name;
                w!("");
                for line in suite.docs.as_deref().unwrap_or_default().lines() {
                    w!("/// {line}");
                }
                w!("#[cfg(test)]");
                w!("mod {suite_name} {{");
                {
                    w!("use ::httpwg::{suite_name} as __suite;");
                    for group in &suite.groups {
                        let group_name = &group.name;
                        w!("");
                        for line in group.docs.as_deref().unwrap_or_default().lines() {
                            w!("/// {line}");
                        }
                        w!("mod {group_name} {{");
                        {
                            w!("use super::__suite::{group_name} as __group;");
                            for test in &group.tests {
                                let test_name = &test.name;
                                w!("");
                                for line in test.docs.as_deref().unwrap_or_default().lines() {
                                    w!("/// {line}");
                                }
                                w!("#[test]");
                                w!("fn {test_name}() {{");
                                {
                                    w!("use __group::{test_name} as test;");
                                    w!("$body");
                                }
                                w!("}}");
                            }
                        }
                        w!("}}");
                    }
                }
                w!("}}");
            }
            w!("}}");
        }
        w!("}}");

        out.flush().unwrap();
    }

    let status = child
        .wait()
        .unwrap_or_else(|err| panic!("{err} while waiting for command: {final_cmd}"));

    if !status.success() {
        eprintln!("command failed: {final_cmd}");
        eprintln!("=== stderr");
        let stderr = stderr.join().unwrap();
        for line in stderr {
            eprintln!("{line}");
        }
        eprintln!("===");
        panic!("command returned status {status:?}, command was: {final_cmd}")
    }

    // Make sure stdout finished successfully
    stdout.join().unwrap();

    println!("✨ httpwg-macros generated!");
}
