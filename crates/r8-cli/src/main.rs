use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "r8", version, about = "R8 clean-core JavaScript engine")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Eval { source: String },
    Selftest,
    Version,
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Eval { source } => match r8_api::eval_i64(&source) {
            Ok(value) => println!("{value}"),
            Err(error) => {
                eprintln!("r8: {error}");
                std::process::exit(1);
            }
        },
        Command::Selftest => {
            let mut failures = Vec::new();
            for (source, expected) in [("1+1", 2), ("2+3+4", 9), ("42", 42)] {
                match r8_api::eval_i64(source) {
                    Ok(value) if value == expected => {}
                    Ok(value) => failures.push(format!("{source} => {value}, expected {expected}")),
                    Err(error) => failures.push(format!("{source} => {error}")),
                }
            }
            if std::mem::size_of::<r8_abi::Value>() != 16 {
                failures.push("Value layout is not 16 bytes".to_string());
            }
            if std::mem::size_of::<r8_abi::Instruction>() != 8 {
                failures.push("Instruction layout is not 8 bytes".to_string());
            }
            let abi_version = unsafe { r8_abi::r8_abi_version() };
            if abi_version != r8_abi::ABI_VERSION {
                failures.push("ABI version mismatch".to_string());
            }
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
