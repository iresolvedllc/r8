use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "r8", version, about = "R8 clean-core JavaScript engine")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Evaluate a single expression and print its value.
    Eval {
        source: String,
    },
    /// Execute a file as a program of expression statements.
    Run {
        path: PathBuf,
    },
    Selftest,
    Version,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Eval { source } => match r8_api::eval(&source) {
            Ok(value) => println!("{value}"),
            Err(error) => {
                eprintln!("r8: {error}");
                std::process::exit(1);
            }
        },
        Command::Run { path } => {
            let source = match std::fs::read_to_string(&path) {
                Ok(source) => source,
                Err(error) => {
                    eprintln!("r8: cannot read {}: {error}", path.display());
                    std::process::exit(1);
                }
            };
            match r8_api::eval_program(&source) {
                // Statement side effects (console.log) have already printed.
                Ok(_) => {}
                Err(error) => {
                    eprintln!("r8: {error}");
                    std::process::exit(1);
                }
            }
        }
        Command::Selftest => {
            let mut failures = Vec::new();
            for (source, expected) in [
                ("1+1", "2"),
                ("2+3+4", "9"),
                ("42", "42"),
                ("0.1+0.2", "0.30000000000000004"),
            ] {
                check_eval(&mut failures, source, expected);
            }
            for (source, expected) in [
                (
                    "let i = 0; let total = 0; while (i < 5) { total += i; i += 1; } total",
                    "10",
                ),
                (
                    "let total = 0; for (let i = 0; i < 10; i += 1) { total += i; } total",
                    "45",
                ),
                ("let x = 0; if (2 > 1) { x = 10; } else { x = 20; } x", "10"),
                ("let x = 1; { let x = 2; } x", "1"),
                ("0 && 5", "0"),
                ("0 || 7", "7"),
            ] {
                check_program(&mut failures, source, expected);
            }
            check_layout(&mut failures);
            check_conformance(&mut failures);
            if failures.is_empty() {
                println!("r8 selftest: ok (abi {})", r8_abi::ABI_VERSION);
            } else {
                for failure in &failures {
                    eprintln!("r8 selftest: {failure}");
                }
                std::process::exit(1);
            }
        }
        Command::Version => {
            println!(
                "r8 {} (abi {})",
                env!("CARGO_PKG_VERSION"),
                r8_abi::ABI_VERSION
            );
        }
    }
}

fn check_eval(failures: &mut Vec<String>, source: &str, expected: &str) {
    match r8_api::eval(source) {
        Ok(value) => {
            let actual = value.to_string();
            if actual != expected {
                failures.push(format!("{source} => {actual}, expected {expected}"));
            }
        }
        Err(error) => failures.push(format!("{source} => {error}")),
    }
}

fn check_program(failures: &mut Vec<String>, source: &str, expected: &str) {
    match r8_api::eval_program(source) {
        Ok(value) => {
            let actual = value.to_string();
            if actual != expected {
                failures.push(format!("{source} => {actual}, expected {expected}"));
            }
        }
        Err(error) => failures.push(format!("{source} => {error}")),
    }
}

fn check_layout(failures: &mut Vec<String>) {
    if std::mem::size_of::<r8_abi::Value>() != 16 {
        failures.push("Value layout is not 16 bytes".to_string());
    }
    if std::mem::size_of::<r8_abi::Instruction>() != 8 {
        failures.push("Instruction layout is not 8 bytes".to_string());
    }
    // SAFETY: pure query of the linked machine's ABI version.
    let abi_version = unsafe { r8_abi::r8_abi_version() };
    if abi_version != r8_abi::ABI_VERSION {
        failures.push(format!(
            "ABI version mismatch: machine reports {abi_version}, rust expects {}",
            r8_abi::ABI_VERSION
        ));
    }
}

fn check_conformance(failures: &mut Vec<String>) {
    // SAFETY: pure table reads exported by the Zig machine; indices stay in
    // range because counts are compared before indexing.
    let tag_count = unsafe { r8_abi::r8_abi_tag_count() };
    if tag_count != r8_abi::CONFORMANCE_TAGS.len() {
        failures.push(format!(
            "tag table sizes differ: machine {tag_count}, rust {}",
            r8_abi::CONFORMANCE_TAGS.len()
        ));
    }
    for (index, expected) in r8_abi::CONFORMANCE_TAGS.iter().enumerate().take(tag_count) {
        // SAFETY: index is below the machine's reported count.
        let actual = unsafe { r8_abi::r8_abi_tag_at(index) };
        if actual != *expected {
            failures.push(format!("tag constant {index} drifted: machine {actual}"));
        }
    }
    let op_count = unsafe { r8_abi::r8_abi_op_count() };
    if op_count != r8_abi::CONFORMANCE_OPS.len() {
        failures.push(format!(
            "opcode table sizes differ: machine {op_count}, rust {}",
            r8_abi::CONFORMANCE_OPS.len()
        ));
    }
    for (index, expected) in r8_abi::CONFORMANCE_OPS.iter().enumerate().take(op_count) {
        // SAFETY: index is below the machine's reported count.
        let actual = unsafe { r8_abi::r8_abi_op_at(index) };
        if actual != *expected {
            failures.push(format!("opcode constant {index} drifted: machine {actual}"));
        }
    }
}
