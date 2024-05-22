use std::{
    io::{BufRead, BufReader, Read},
    process::{Command, Stdio},
};

mod ast;

fn main() {
    println!("🧱 Generating rustdoc...");

    let mut cmd = Command::new("cargo");
    cmd.arg("rustdoc");
    cmd.args(["-Z", "unstable-options"]);
    cmd.args(["--output-format", "json"]);
    cmd.args(["--package", "httpwg"]);
    cmd.args(["--target-dir", "target-codegen"]);
    cmd.arg("--frozen");
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
        groups: Vec<Group>,
    }

    #[derive(Debug)]
    struct Group {
        name: String,
        tests: Vec<Test>,
    }

    #[derive(Debug)]
    struct Test {
        name: String,
    }

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
                println!("[{item_id}] Suite {suite_name}");
                let mut suite = Suite {
                    name: suite_name,
                    groups: Default::default(),
                };

                for item_id in &module.items {
                    let item = doc.index.get(item_id).expect("Could not find some node");
                    match &item.inner {
                        ast::ItemInner::Module(module) => {
                            let group_name = item.name.clone().unwrap();
                            if group_name.starts_with("_") {
                                // good!
                            } else {
                                // skip
                                continue;
                            }
                            println!("[{item_id}] Group {group_name}");

                            let mut group = Group {
                                name: group_name,
                                tests: Default::default(),
                            };

                            for item_id in &module.items {
                                let item =
                                    doc.index.get(item_id).expect("Could not find some node");
                                match &item.inner {
                                    ast::ItemInner::Function(f) => {
                                        let test_name = item.name.clone().unwrap();
                                        println!("[{item_id}] Test {test_name}");

                                        let test = Test { name: test_name };
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
            }
            _ => {
                // ignore
            }
        }
    }

    println!("🦉 TODO: the rest of the owl");
    // println!("✨ httpwg-macros generated!");
}
