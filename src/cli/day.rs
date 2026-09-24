use crate::api::schema::{
    DayAddParams, DayBindParams, DayItemKind, DayItemSource, DayItemTarget, DayLinkParams,
    DayListParams, DayNoteParams, Method, PaneCurrentParams, Request,
};

pub(super) fn run_day_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("add") => day_add(&args[1..]),
        Some("list") => day_list(&args[1..]),
        Some("bind") => day_bind(&args[1..]),
        Some("link") => day_link(&args[1..]),
        Some("note") => day_note(&args[1..]),
        Some("done") => day_target(&args[1..], true),
        Some("dismiss") => day_target(&args[1..], false),
        Some("help" | "--help" | "-h") => {
            print_day_help();
            Ok(0)
        }
        _ => {
            print_day_help();
            Ok(2)
        }
    }
}

fn day_add(args: &[String]) -> std::io::Result<i32> {
    let params = match parse_add(args) {
        Ok(params) => params,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };
    send("cli:day:add", Method::DayAdd(params))
}

fn day_list(args: &[String]) -> std::io::Result<i32> {
    let (json, include_dismissed) = match parse_list(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };
    let response = super::send_request(&Request {
        id: "cli:day:list".into(),
        method: Method::DayList(DayListParams { include_dismissed }),
    })?;
    if response.get("error").is_some() {
        return super::print_response(&response);
    }
    let result = &response["result"];
    if json {
        println!("{}", serde_json::to_string(result)?);
        return Ok(0);
    }
    for error in result["load_errors"].as_array().into_iter().flatten() {
        eprintln!(
            "{}: {}",
            error["path"].as_str().unwrap_or("day item"),
            error["message"].as_str().unwrap_or("cannot load")
        );
    }
    println!("{:<20} {:<9} {:<8} TITLE", "ID", "COLUMN", "STALE");
    for item in result["items"].as_array().into_iter().flatten() {
        println!(
            "{:<20} {:<9} {:<8} {}",
            item["id"].as_str().unwrap_or("-"),
            item["column"].as_str().unwrap_or("todo"),
            if item["stale"].as_bool().unwrap_or(false) {
                "yes"
            } else {
                "no"
            },
            item["title"].as_str().unwrap_or("")
        );
        if let Some(notice) = item["notice"].as_str() {
            println!("  {notice}");
        }
    }
    Ok(0)
}

fn day_bind(args: &[String]) -> std::io::Result<i32> {
    // `HERDR_PANE_ID` names a pane of the server this process was started under.
    // Once `--session` points the request at a different server the same public
    // id belongs to an unrelated pane, so the inherited value means nothing
    // there. Naming the session it is already in still addresses its own panes,
    // and discarding the caller there would fall back to whichever pane happens
    // to be focused.
    let caller_pane = if crate::session::addresses_own_server() {
        super::target::caller_pane_id()
    } else {
        None
    };
    let (id, pane_id) = match parse_bind(args, caller_pane) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };
    let pane_id = match pane_id {
        Some(pane_id) => pane_id,
        // Report a missing pane the way every other argument problem is
        // reported, rather than as a debug-formatted io error.
        None => match current_pane_id() {
            Ok(pane_id) => pane_id,
            Err(message) => {
                eprintln!("{message}");
                return Ok(2);
            }
        },
    };
    send(
        "cli:day:bind",
        Method::DayBind(DayBindParams { id, pane_id }),
    )
}

fn current_pane_id() -> Result<String, String> {
    let response = super::send_request(&Request {
        id: "cli:day:bind:current".into(),
        method: Method::PaneCurrent(PaneCurrentParams {
            caller_pane_id: None,
        }),
    })
    .map_err(|error| format!("cannot reach the server: {error}"))?;
    response["result"]["pane"]["pane_id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| {
            "cannot resolve the current pane; name one as `herdr day bind <id> <pane>`".to_string()
        })
}

fn day_link(args: &[String]) -> std::io::Result<i32> {
    let params = match parse_link(args) {
        Ok(params) => params,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };
    send("cli:day:link", Method::DayLink(params))
}

fn day_note(args: &[String]) -> std::io::Result<i32> {
    let params = match args {
        [id, clear] if clear == "--clear" => DayNoteParams {
            id: id.clone(),
            note: None,
        },
        [id, note] => DayNoteParams {
            id: id.clone(),
            note: Some(note.clone()),
        },
        _ => {
            eprintln!("usage: herdr day note <id> <text>|--clear");
            return Ok(2);
        }
    };
    send("cli:day:note", Method::DayNote(params))
}

fn day_target(args: &[String], done: bool) -> std::io::Result<i32> {
    let [id] = args else {
        eprintln!(
            "usage: herdr day {} <id>",
            if done { "done" } else { "dismiss" }
        );
        return Ok(2);
    };
    let target = DayItemTarget { id: id.clone() };
    send(
        if done {
            "cli:day:done"
        } else {
            "cli:day:dismiss"
        },
        if done {
            Method::DayDone(target)
        } else {
            Method::DayDismiss(target)
        },
    )
}

fn send(id: &str, method: Method) -> std::io::Result<i32> {
    super::print_response(&super::send_request(&Request {
        id: id.to_string(),
        method,
    })?)
}

fn parse_add(args: &[String]) -> Result<DayAddParams, String> {
    let Some(title) = args.first().cloned() else {
        return Err("usage: herdr day add <title> [--kind task|noticed] [--note TEXT]".into());
    };
    let mut kind = DayItemKind::Task;
    let mut note = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--kind" => {
                let value = args
                    .get(index + 1)
                    .ok_or_else(|| "missing value for --kind".to_string())?;
                kind = match value.as_str() {
                    "task" => DayItemKind::Task,
                    "noticed" => DayItemKind::Noticed,
                    _ => return Err(format!("invalid kind: {value} (expected task or noticed)")),
                };
                index += 2;
            }
            "--note" => {
                note = Some(
                    args.get(index + 1)
                        .ok_or_else(|| "missing value for --note".to_string())?
                        .clone(),
                );
                index += 2;
            }
            option => return Err(format!("unknown option: {option}")),
        }
    }
    Ok(DayAddParams {
        title,
        kind,
        source: DayItemSource::Manual,
        note,
    })
}

fn parse_list(args: &[String]) -> Result<(bool, bool), String> {
    let mut json = false;
    let mut include_dismissed = false;
    for arg in args {
        match arg.as_str() {
            "--json" => json = true,
            "--all" => include_dismissed = true,
            option => return Err(format!("unknown option: {option}")),
        }
    }
    Ok((json, include_dismissed))
}

fn parse_bind(
    args: &[String],
    caller_pane_id: Option<String>,
) -> Result<(String, Option<String>), String> {
    let Some(id) = args.first().cloned() else {
        return Err("usage: herdr day bind <id> [pane_id|--pane ID|--current]".into());
    };
    let pane_id = match &args[1..] {
        [] => caller_pane_id,
        [pane_id] if pane_id != "--current" => Some(super::normalize_pane_id(pane_id)),
        [current] if current == "--current" => caller_pane_id,
        [flag, pane_id] if flag == "--pane" => Some(super::normalize_pane_id(pane_id)),
        _ => return Err("usage: herdr day bind <id> [pane_id|--pane ID|--current]".into()),
    };
    Ok((id, pane_id))
}

fn parse_link(args: &[String]) -> Result<DayLinkParams, String> {
    let Some(id) = args.first().cloned() else {
        return Err("usage: herdr day link <id> [--ticket ID] [--pr URL]".into());
    };
    let mut ticket = None;
    let mut pr = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--ticket" => {
                ticket = Some(
                    args.get(index + 1)
                        .ok_or_else(|| "missing value for --ticket".to_string())?
                        .clone(),
                );
                index += 2;
            }
            "--pr" => {
                pr = Some(
                    args.get(index + 1)
                        .ok_or_else(|| "missing value for --pr".to_string())?
                        .clone(),
                );
                index += 2;
            }
            option => return Err(format!("unknown option: {option}")),
        }
    }
    if ticket.is_none() && pr.is_none() {
        return Err("day link requires --ticket or --pr".into());
    }
    Ok(DayLinkParams { id, ticket, pr })
}

fn print_day_help() {
    eprintln!("herdr day commands:");
    eprintln!("  herdr day add <title> [--kind task|noticed] [--note TEXT]");
    eprintln!("  herdr day list [--json] [--all]");
    eprintln!("  herdr day bind <id> [pane_id|--pane ID|--current]");
    eprintln!("  herdr day link <id> [--ticket ID] [--pr URL]");
    eprintln!("  herdr day note <id> <text>|--clear");
    eprintln!("  herdr day done <id>");
    eprintln!("  herdr day dismiss <id>");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn add_parses_noticed_kind_and_note() {
        let parsed = parse_add(&args(&[
            "Clean up retries",
            "--kind",
            "noticed",
            "--note",
            "after release",
        ]))
        .expect("parse add");
        assert_eq!(parsed.title, "Clean up retries");
        assert_eq!(parsed.kind, DayItemKind::Noticed);
        assert_eq!(parsed.note.as_deref(), Some("after release"));
    }

    #[test]
    fn bind_prefers_explicit_pane_and_defaults_to_caller_pane() {
        assert_eq!(
            parse_bind(&args(&["item-1"]), Some("w_main:p1".into())).expect("caller pane"),
            ("item-1".into(), Some("w_main:p1".into()))
        );
        assert_eq!(
            parse_bind(
                &args(&["item-1", "--pane", "w_other:p2"]),
                Some("w_main:p1".into())
            )
            .expect("explicit pane"),
            ("item-1".into(), Some("w_other:p2".into()))
        );
    }

    #[test]
    fn list_json_requests_structured_derived_items() {
        assert_eq!(parse_list(&args(&["--json"])), Ok((true, false)));
    }
}
