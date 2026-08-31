use std::io;
use std::time::Instant;

use crate::config::Config;
use crate::work_index::{refresh_work_index, WorkItem};

pub(super) fn run_work_index_command(args: &[String]) -> io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("list") => list_work_index(&args[1..]),
        Some("help" | "--help" | "-h") => {
            print_work_index_help();
            Ok(0)
        }
        _ => {
            print_work_index_help();
            Ok(2)
        }
    }
}

#[derive(Debug, Default)]
struct ListOptions {
    json: bool,
}

fn list_work_index(args: &[String]) -> io::Result<i32> {
    let help_requested = args.contains(&"--help".to_string()) || args.contains(&"-h".to_string());
    if help_requested
        && args
            .iter()
            .all(|arg| matches!(arg.as_str(), "--json" | "--help" | "-h"))
    {
        println!("List indexed work items\n\nUsage: herdr work-index list [OPTIONS]\n\nOptions:\n      --json  Print structured rows as JSON\n      -h, --help  Print help");
        return Ok(0);
    }
    let options = match args {
        [] => ListOptions::default(),
        [flag] if flag == "--json" => ListOptions { json: true },
        _ => {
            eprintln!("usage: herdr work-index list [--json]");
            return Ok(2);
        }
    };
    let config = Config::load().config;
    if !config.work_index.enabled {
        eprintln!("work index disabled. Set [work_index] enabled = true to collect it.");
        return Ok(0);
    }
    let snapshot = refresh_work_index(
        &config.work_index,
        &[],
        Instant::now(),
        Instant::now() + crate::work_index::WORK_INDEX_BATCH_TIMEOUT,
        crate::work_index::WORK_INDEX_TARGET_TIMEOUT,
        std::path::Path::new("gh"),
        std::path::Path::new("linearis"),
    );
    if let Some(unavailable) = snapshot.unavailable.as_deref() {
        eprintln!("work index unavailable: {unavailable}");
    }
    let rows = snapshot.items;
    if options.json {
        println!("{}", serde_json::to_string(&rows)?);
    } else {
        print_table(&rows);
    }
    Ok(0)
}

fn print_table(rows: &[WorkItem]) {
    println!(
        "{:<18} {:<8} {:<36} {:<18} STATE",
        "REPO", "PR", "TITLE", "TICKETS"
    );
    for row in rows {
        let pr = row
            .pr_number
            .map_or_else(|| "-".into(), |number| format!("#{number}"));
        let title = row
            .pr_title
            .as_deref()
            .unwrap_or_else(|| row.ticket_title.as_deref().unwrap_or("unbound work"));
        let tickets = if row.ticket_ids.is_empty() {
            "no ticket".into()
        } else {
            row.ticket_ids.join(",")
        };
        let state = row
            .pr_state
            .as_deref()
            .or(row.ticket_state.as_deref())
            .unwrap_or("-");
        println!(
            "{:<18} {:<8} {:<36} {:<18} {}",
            row.repo, pr, title, tickets, state
        );
    }
}

fn print_work_index_help() {
    eprintln!("herdr work-index commands:");
    eprintln!("  herdr work-index list [--json]");
}
