//! `arbit-analytics` — the off-line backtest / golden-replay regression CLI (observ-10/11/12).
//!
//! Subcommands:
//! * `gate <corpus.json> [tolerance_bps]` — replay the corpus through the LIVE sizing mirror +
//!   `CostModel`; exit **nonzero** if any sample deviates beyond tolerance (CI-blocking before any
//!   capital-committing deploy).
//! * `replay <corpus.json> [tolerance_bps]` — print the per-sample replay report (always exit 0).
//! * `backtest <corpus.json>` — print the aggregate unit-economics confirmation report.
//!
//! It links `arb-bot` so the replay reuses the SAME bit-exact `arb-math` mirror + `CostModel` the
//! bot signs against — predicted here == predicted live (observ-11).

mod backtest;
mod corpus;
mod replay;
mod report;

use std::path::Path;
use std::process::ExitCode;

use arb_bot::metrics::econ::{CostModel, EconParams};

use crate::corpus::load_corpus;

const DEFAULT_TOLERANCE_BPS: i64 = 1;

fn usage() {
    eprintln!(
        "usage:\n  \
         arbit-analytics gate <corpus.json> [tolerance_bps]   # CI gate, nonzero exit on drift\n  \
         arbit-analytics replay <corpus.json> [tolerance_bps] # per-sample replay report\n  \
         arbit-analytics backtest <corpus.json>               # unit-economics confirmation\n  \
         arbit-analytics                                      # sample round-trip demo"
    );
}

fn parse_tolerance(arg: Option<&String>) -> i64 {
    arg.and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_TOLERANCE_BPS)
}

fn run() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let model = CostModel::new(EconParams::default());

    match args.get(1).map(String::as_str) {
        Some("gate") => {
            let Some(path) = args.get(2) else {
                usage();
                return ExitCode::FAILURE;
            };
            let tolerance = parse_tolerance(args.get(3));
            let samples = match load_corpus(Path::new(path)) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
            };
            let results = replay::replay(&samples, tolerance, &model);
            println!("{}", report::render_replay(&results));
            eprintln!("{}", report::replay_summary_line(&results));
            if replay::all_within_tolerance(&results) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE // block the deploy
            }
        }
        Some("replay") => {
            let Some(path) = args.get(2) else {
                usage();
                return ExitCode::FAILURE;
            };
            let tolerance = parse_tolerance(args.get(3));
            match load_corpus(Path::new(path)) {
                Ok(samples) => {
                    let results = replay::replay(&samples, tolerance, &model);
                    println!("{}", report::render_replay(&results));
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("backtest") => {
            let Some(path) = args.get(2) else {
                usage();
                return ExitCode::FAILURE;
            };
            match load_corpus(Path::new(path)) {
                Ok(samples) => {
                    let report = backtest::run_backtest(&samples, &model);
                    println!("{}", report::render_backtest(&report));
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
        None => {
            // Back-compat: prove the analytics binary links the same gate-critical math.
            demo();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown subcommand: {other}");
            usage();
            ExitCode::FAILURE
        }
    }
}

/// A fixed round-trip re-priced through the bot's `arb-math` (links-the-same-math smoke).
fn demo() {
    use arb_math::{size_round_trip, CpmmReserves, RoundTrip, SizingPolicy};
    use arb_types::SwapDir;
    let a = CpmmReserves::new(1_000_000, 2_000_000, 25, 10_000);
    let b = CpmmReserves::new(2_000_000, 1_100_000, 25, 10_000);
    let rt = RoundTrip::new(a, SwapDir::AtoB, b, SwapDir::AtoB);
    match size_round_trip(&rt, SizingPolicy::DEFAULT) {
        Some((delta, out, profit)) => {
            println!("sample round-trip: delta_in={delta} predicted_out={out} profit={profit}");
        }
        None => println!("sample round-trip: no profitable size"),
    }
    eprintln!("(run `arbit-analytics gate <corpus.json>` for the regression gate)");
}

fn main() -> ExitCode {
    run()
}
