//! The command-line entry point: `payments-engine transactions.csv > accounts.csv`.
//!
//! The accounts go to stdout and the metrics to stderr. Exit codes (A14):
//!
//! - `0`: the file was processed, even if some rows were rejected;
//! - `1`: the input can't be read, or the output can't be written;
//! - `2`: the arguments are wrong.

use std::env;
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

use payments_engine::metrics::MemoryMetrics;

fn main() -> ExitCode {
    // The input file is the first and only argument.
    let mut args = env::args_os().skip(1);
    let (Some(path), None) = (args.next(), args.next()) else {
        eprintln!("usage: payments-engine <transactions.csv>");
        return ExitCode::from(2);
    };
    let path = PathBuf::from(path);
    let input = match File::open(&path) {
        Ok(file) => file,
        Err(err) => {
            eprintln!("error: can't open {}: {err}", path.display());
            return ExitCode::FAILURE;
        }
    };

    let mut metrics = MemoryMetrics::default();
    let result = payments_engine::run(input, io::stdout().lock(), &mut metrics);
    eprint!("{metrics}");
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
