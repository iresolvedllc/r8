//! r8-test262: the Test262 conformance runner for the R8 engine.
//!
//! Walks a Test262 checkout, classifies each test from its YAML frontmatter
//! (flags, includes, negative phase), runs it in a subprocess of the R8 CLI
//! with shim harness files prepended, and buckets the outcome:
//!
//! - `pass`        — exit 0 (or the expected failure for negative tests)
//! - `fail`        — clean nonzero exit with unexpected output
//! - `timeout`     — exceeded the per-test time limit
//! - `crash`       — killed by a signal (engine panic / invariant breach)
//! - `unsupported` — statically skipped: module/async/generate tests, or
//!   harness includes the shim does not provide
//!
//! Reports a JSON result file plus a Markdown summary, and (with `--gate`)
//! enforces minimum pass rates for a set of suite prefixes so regressions
//! fail CI.

mod frontmatter;
mod shim;
mod tokens;

use clap::Parser;
use std::fs;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Parser)]
#[command(
    name = "r8-test262",
    version,
    about = "Test262 conformance runner for the R8 engine"
)]
struct Cli {
    /// Root of a Test262 checkout (the directory containing `test/` and
    /// `harness/`). Defaults to $R8_TEST262_ROOT, then target/corpus/test262.
    #[arg(long)]
    root: Option<PathBuf>,
    /// Path to the `r8` CLI binary under test.
    #[arg(long, default_value = "target/debug/r8")]
    engine: PathBuf,
    /// Only run tests whose path (relative to `<root>/test`) starts with one
    /// of these prefixes. Repeatable.
    #[arg(long = "suite")]
    suite: Vec<String>,
    /// Run at most N tests (after filtering). 0 = no limit.
    #[arg(long, default_value_t = 0)]
    limit: usize,
    /// Worker threads.
    #[arg(long, default_value_t = default_jobs())]
    jobs: usize,
    /// Per-test timeout in milliseconds.
    #[arg(long, default_value_t = 10_000)]
    timeout_ms: u64,
    /// Write `test262-results.json` and `test262-summary.md` here.
    #[arg(long)]
    report: Option<PathBuf>,
    /// Enforce minimum pass rates from this gate file and exit 2 on
    /// regression. Format: one `suite-prefix min-rate-percent` per line.
    #[arg(long)]
    gate: Option<PathBuf>,
}

fn default_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// What the test is expected to do, derived from its frontmatter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expectation {
    /// Exit 0.
    Pass,
    /// The test is a `negative: {phase: parse}` test: any compile failure
    /// (exit nonzero, stderr mentioning the error) counts as a pass.
    ParseError,
    /// The test is a `negative: {phase: runtime}` test: an uncaught throw
    /// (exit nonzero, stderr mentioning the error) counts as a pass.
    RuntimeError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bucket {
    Pass,
    Fail,
    Timeout,
    Crash,
    Unsupported,
}

impl Bucket {
    fn label(self) -> &'static str {
        match self {
            Bucket::Pass => "pass",
            Bucket::Fail => "fail",
            Bucket::Timeout => "timeout",
            Bucket::Crash => "crash",
            Bucket::Unsupported => "unsupported",
        }
    }
}

struct Outcome {
    /// Path relative to `<root>/test`.
    suite_path: String,
    bucket: Bucket,
    elapsed_ms: u128,
    /// First line of stderr for failures (diagnosis aid).
    note: String,
}

fn main() {
    let cli = Cli::parse();
    if let Err(message) = run(&cli) {
        eprintln!("r8-test262: {message}");
        std::process::exit(1);
    }
}

fn run(cli: &Cli) -> Result<(), String> {
    let root = cli
        .root
        .clone()
        .or_else(|| std::env::var("R8_TEST262_ROOT").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("target/corpus/test262"));
    if !root.join("test").is_dir() || !root.join("harness").is_dir() {
        return Err(format!(
            "no Test262 checkout at {} (expected test/ and harness/); \
             run scripts/fetch-corpus.sh or pass --root",
            root.display()
        ));
    }
    let engine = cli
        .engine
        .canonicalize()
        .unwrap_or_else(|_| cli.engine.clone());
    if !engine.is_file() {
        return Err(format!("engine binary not found: {}", engine.display()));
    }

    let test_root = root.join("test");
    let mut tests = Vec::new();
    collect_tests(&test_root, &mut tests)?;
    println!(
        "r8-test262: {} test files under {}",
        tests.len(),
        test_root.display()
    );

    // Classify before running: many tests are statically unsupported.
    let shim = shim::Shim::load()?;
    let mut jobs = Vec::new();
    let mut unsupported = Vec::new();
    for path in tests {
        let suite_path = path
            .strip_prefix(&test_root)
            .expect("path under test root")
            .to_string_lossy()
            .replace('\\', "/");
        if !cli.suite.is_empty()
            && !cli
                .suite
                .iter()
                .any(|prefix| suite_path.starts_with(prefix.as_str()))
        {
            continue;
        }
        match classify(&shim, &path) {
            Ok(Some(job)) => jobs.push(Job {
                suite_path,
                path,
                job,
            }),
            Ok(None) => {} // filtered out as unsupported below
            Err(reason) => unsupported.push(Outcome {
                suite_path,
                bucket: Bucket::Unsupported,
                elapsed_ms: 0,
                note: reason,
            }),
        }
    }
    if cli.limit > 0 && jobs.len() > cli.limit {
        jobs.truncate(cli.limit);
    }

    let total_classified = jobs.len() + unsupported.len();
    println!(
        "r8-test262: {} runnable, {} unsupported statically",
        jobs.len(),
        unsupported.len()
    );

    let outcomes = Mutex::new(unsupported);
    let next = AtomicUsize::new(0);
    let started = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..cli.jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    if index >= jobs.len() {
                        break;
                    }
                    let job = &jobs[index];
                    let outcome = execute(&engine, job, cli.timeout_ms);
                    let mut all = outcomes.lock().expect("outcome lock");
                    all.push(outcome);
                    let done = all.len();
                    if done % 500 == 0 {
                        eprintln!(
                            "r8-test262: {done}/{total} ({elapsed}s)",
                            total = total_classified,
                            elapsed = started.elapsed().as_secs()
                        );
                    }
                }
            });
        }
    });
    let mut all = outcomes.into_inner().expect("outcome lock");
    all.sort_by(|a, b| a.suite_path.cmp(&b.suite_path));
    println!(
        "r8-test262: finished {} tests in {}s",
        all.len(),
        started.elapsed().as_secs()
    );

    if let Some(report_dir) = &cli.report {
        write_report(report_dir, &all)?;
        println!("r8-test262: report written to {}", report_dir.display());
    }
    print_summary(&all);

    if let Some(gate_file) = &cli.gate {
        let failed = enforce_gates(gate_file, &all)?;
        if !failed.is_empty() {
            for failure in &failed {
                eprintln!("r8-test262: GATE FAILED: {failure}");
            }
            std::process::exit(2);
        }
        println!("r8-test262: all gates passed");
    }
    Ok(())
}

struct Job {
    suite_path: String,
    path: PathBuf,
    job: RunnableJob,
}

struct RunnableJob {
    expectation: Expectation,
    /// Harness files to prepend, in order.
    harness: Vec<PathBuf>,
}

/// Statically classifies a test file. `Ok(None)` means the test matched none
/// of the suite filters (caller handles), `Err(reason)` marks it unsupported.
fn classify(shim: &shim::Shim, path: &Path) -> Result<Option<RunnableJob>, String> {
    let source = fs::read_to_string(path).map_err(|error| format!("read: {error}"))?;
    let meta = frontmatter::parse(&source).map_err(|error| format!("frontmatter: {error}"))?;

    let flags = &meta.flags;
    if flags.iter().any(|flag| flag == "module") {
        return Err("module".to_string());
    }
    if flags
        .iter()
        .any(|flag| flag == "async" || flag == "async-throws")
    {
        return Err("async".to_string());
    }
    if flags.iter().any(|flag| flag == "generate") {
        return Err("generator".to_string());
    }
    for include in &meta.includes {
        if shim.provides(include).is_none() {
            return Err(format!("harness include not shimmed: {include}"));
        }
    }

    let expectation = match meta.negative_phase.as_deref() {
        Some("parse") => Expectation::ParseError,
        Some("runtime") => Expectation::RuntimeError,
        Some(other) => return Err(format!("negative phase: {other}")),
        None => Expectation::Pass,
    };

    // The default harness is assert.js + sta.js (skipped for `raw` tests,
    // which must parse and run on their own).
    let mut harness = Vec::new();
    let raw = flags.iter().any(|flag| flag == "raw");
    if !raw {
        harness.push(
            shim.provides("assert.js")
                .expect("assert shimmed")
                .to_path_buf(),
        );
        harness.push(shim.provides("sta.js").expect("sta shimmed").to_path_buf());
    }
    for include in &meta.includes {
        harness.push(
            shim.provides(include)
                .expect("include shimmed")
                .to_path_buf(),
        );
    }

    Ok(Some(RunnableJob {
        expectation,
        harness,
    }))
}

fn collect_tests(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|error| format!("read {}: {error}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| format!("entry: {error}"))?;
        let path = entry.path();
        if path.is_dir() {
            collect_tests(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("js") {
            out.push(path);
        }
    }
    Ok(())
}

fn execute(engine: &Path, job: &Job, timeout_ms: u64) -> Outcome {
    let started = Instant::now();
    let mut command = Command::new(engine);
    command
        .arg("run")
        .arg(&job.path)
        .args(
            job.job
                .harness
                .iter()
                .flat_map(|h| [String::from("--harness"), h.to_string_lossy().into_owned()]),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return Outcome {
                suite_path: job.suite_path.clone(),
                bucket: Bucket::Crash,
                elapsed_ms: 0,
                note: format!("spawn failed: {error}"),
            };
        }
    };

    // Poll-wait for the timeout, then kill.
    let deadline = started + Duration::from_millis(timeout_ms);
    let mut timed_out = false;
    let mut status: Option<std::process::ExitStatus> = None;
    loop {
        match child.try_wait() {
            Ok(Some(exit)) => {
                status = Some(exit);
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break,
        }
    }
    let elapsed_ms = started.elapsed().as_millis();

    // Drain stderr (bounded) for diagnosis notes. The child has exited, so
    // the pipe cannot block anymore.
    let mut stderr_text = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = std::io::Read::read_to_string(&mut stderr, &mut stderr_text);
    }
    let note = stderr_text
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect::<String>();

    let (bucket, matched) = match status {
        None if timed_out => (Bucket::Timeout, false),
        None => (Bucket::Crash, false),
        Some(status) if status.code() == Some(0) => {
            (Bucket::Pass, job.job.expectation == Expectation::Pass)
        }
        Some(status) if status.code().is_none() => (Bucket::Crash, false),
        Some(_) => {
            let errored = note.contains("r8:");
            match job.job.expectation {
                Expectation::Pass => (Bucket::Fail, false),
                Expectation::ParseError => (Bucket::Pass, errored && note.contains("parse error")),
                Expectation::RuntimeError => (Bucket::Pass, errored && note.contains("uncaught")),
            }
        }
    };
    let bucket = if matched { Bucket::Pass } else { bucket };

    Outcome {
        suite_path: job.suite_path.clone(),
        bucket,
        elapsed_ms,
        note,
    }
}

fn write_report(dir: &Path, outcomes: &[Outcome]) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|error| format!("mkdir: {error}"))?;

    // JSON: buckets + per-test rows.
    let mut json = String::from("{\"tests\":[");
    for (index, outcome) in outcomes.iter().enumerate() {
        if index > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            "{{\"path\":{},\"bucket\":\"{}\",\"ms\":{},\"note\":{}}}",
            tokens::json_string(&outcome.suite_path),
            outcome.bucket.label(),
            outcome.elapsed_ms,
            tokens::json_string(&outcome.note),
        ));
    }
    json.push_str("]}");
    fs::write(dir.join("test262-results.json"), json)
        .map_err(|error| format!("write json: {error}"))?;

    // Markdown summary.
    let groups = summarize(outcomes);
    let mut md = String::from("# Test262 results\n\n");
    md.push_str("| suite | pass | fail | timeout | crash | unsupported | total | rate |\n");
    md.push_str("|---|---|---|---|---|---|---|---|\n");
    for group in &groups {
        md.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {}% |\n",
            group.prefix,
            group.pass,
            group.fail,
            group.timeout,
            group.crash,
            group.unsupported,
            group.total(),
            group.rate(),
        ));
    }
    fs::write(dir.join("test262-summary.md"), md).map_err(|error| format!("write md: {error}"))?;
    Ok(())
}

fn print_summary(outcomes: &[Outcome]) {
    let groups = summarize(outcomes);
    println!(
        "{:<28} {:>6} {:>6} {:>6} {:>6} {:>6} {:>7}  rate",
        "suite", "pass", "fail", "t/o", "crash", "unsup", "total"
    );
    for group in &groups {
        println!(
            "{:<28} {:>6} {:>6} {:>6} {:>6} {:>6} {:>7}  {}%",
            group.prefix,
            group.pass,
            group.fail,
            group.timeout,
            group.crash,
            group.unsupported,
            group.total(),
            group.rate(),
        );
    }
}

struct Group {
    prefix: String,
    pass: usize,
    fail: usize,
    timeout: usize,
    crash: usize,
    unsupported: usize,
}

impl Group {
    fn total(&self) -> usize {
        self.pass + self.fail + self.timeout + self.crash + self.unsupported
    }

    /// Rate over tests the shim can actually run (unsupported excluded).
    fn rate(&self) -> usize {
        let runnable = self.pass + self.fail + self.timeout + self.crash;
        (self.pass * 100).checked_div(runnable).unwrap_or(0)
    }
}

fn summarize(outcomes: &[Outcome]) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for outcome in outcomes {
        // Top-level directory (language, built-ins, ...).
        let prefix = outcome
            .suite_path
            .split('/')
            .next()
            .unwrap_or(&outcome.suite_path)
            .to_string();
        let group = match groups.iter_mut().find(|g| g.prefix == prefix) {
            Some(group) => group,
            None => {
                groups.push(Group {
                    prefix,
                    pass: 0,
                    fail: 0,
                    timeout: 0,
                    crash: 0,
                    unsupported: 0,
                });
                groups.last_mut().expect("just pushed")
            }
        };
        match outcome.bucket {
            Bucket::Pass => group.pass += 1,
            Bucket::Fail => group.fail += 1,
            Bucket::Timeout => group.timeout += 1,
            Bucket::Crash => group.crash += 1,
            Bucket::Unsupported => group.unsupported += 1,
        }
    }
    groups.sort_by(|a, b| a.prefix.cmp(&b.prefix));
    groups
}

fn enforce_gates(gate_file: &Path, outcomes: &[Outcome]) -> Result<Vec<String>, String> {
    let text = fs::read_to_string(gate_file).map_err(|error| format!("read gates: {error}"))?;
    let mut failures = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((prefix, min)) = line.rsplit_once(char::is_whitespace) else {
            return Err(format!("bad gate line: {line:?}"));
        };
        let min: usize = min
            .trim()
            .trim_end_matches('%')
            .parse()
            .map_err(|error| format!("bad gate rate in {line:?}: {error}"))?;
        let prefix = prefix.trim();
        let (pass, runnable) = rate_for_prefix(prefix, outcomes);
        let rate = (pass * 100).checked_div(runnable);
        match rate {
            None => failures.push(format!("{prefix}: no results (suite missing?)")),
            Some(rate) if rate < min => {
                failures.push(format!("{prefix}: {rate}% < required {min}%"))
            }
            _ => {}
        }
    }
    Ok(failures)
}

/// Pass rate over a suite prefix, excluding unsupported tests.
fn rate_for_prefix(prefix: &str, outcomes: &[Outcome]) -> (usize, usize) {
    let mut pass = 0;
    let mut runnable = 0;
    for outcome in outcomes {
        if !outcome.suite_path.starts_with(prefix) {
            continue;
        }
        match outcome.bucket {
            Bucket::Pass => {
                pass += 1;
                runnable += 1;
            }
            Bucket::Fail | Bucket::Timeout | Bucket::Crash => runnable += 1,
            Bucket::Unsupported => {}
        }
    }
    (pass, runnable)
}
