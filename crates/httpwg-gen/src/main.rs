use std::{
    io::{BufRead, BufReader, Read},
    process::{Command, Stdio},
};

fn main() {
    println!("🧱 Generating rustdoc...");

    let mut cmd = Command::new("cargo");
    cmd.arg("rustdoc");
    cmd.args(["-Z", "unstable-options"]);
    cmd.args(["--output-format", "json"]);
    cmd.args(["--package", "httpwg"]);
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
}
