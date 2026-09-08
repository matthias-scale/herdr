use crate::api::schema::{EmptyParams, Method, Request, ThemeSetParams};
use crate::config::HostAppearanceOverride;

pub(super) fn run_theme_command(args: &[String]) -> std::io::Result<i32> {
    match args.first().map(String::as_str) {
        Some("status") => theme_status(&args[1..]),
        Some("set") => theme_set(&args[1..]),
        Some("help" | "--help" | "-h") => {
            print_theme_help();
            Ok(0)
        }
        _ => {
            print_theme_help();
            Ok(2)
        }
    }
}

fn theme_status(args: &[String]) -> std::io::Result<i32> {
    let json = match args {
        [] => false,
        [flag] if flag == "--json" => true,
        _ => {
            eprintln!("usage: herdr theme status [--json]");
            return Ok(2);
        }
    };
    let response = super::send_request(&Request {
        id: "cli:theme:status".into(),
        method: Method::ThemeStatus(EmptyParams::default()),
    })?;
    if json || response.get("error").is_some() {
        return super::print_response(&response);
    }

    let result = &response["result"];
    println!(
        "host reported: {}",
        result["host_reported"].as_str().unwrap_or("unknown")
    );
    println!(
        "override: {}",
        result["override"].as_str().unwrap_or("unknown")
    );
    println!(
        "effective appearance: {}",
        result["effective_appearance"].as_str().unwrap_or("unknown")
    );
    println!(
        "theme: {}",
        result["theme_name"].as_str().unwrap_or("unknown")
    );
    Ok(0)
}

fn theme_set(args: &[String]) -> std::io::Result<i32> {
    let appearance = match args {
        [value] if value == "light" => HostAppearanceOverride::Light,
        [value] if value == "dark" => HostAppearanceOverride::Dark,
        [value] if value == "auto" => HostAppearanceOverride::Auto,
        _ => {
            eprintln!("usage: herdr theme set light|dark|auto");
            return Ok(2);
        }
    };
    let response = super::send_request(&Request {
        id: "cli:theme:set".into(),
        method: Method::ThemeSet(ThemeSetParams {
            host_appearance: appearance,
        }),
    })?;
    if response.get("error").is_some() {
        return super::print_response(&response);
    }
    println!("theme appearance override set to {}", appearance.as_str());
    Ok(0)
}

fn print_theme_help() {
    eprintln!("herdr theme commands:");
    eprintln!("  herdr theme status [--json]       show the running theme appearance");
    eprintln!("  herdr theme set light|dark|auto   override the running host appearance");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_set_accepts_only_one_supported_appearance() {
        for value in ["light", "dark", "auto"] {
            let appearance = match value {
                "light" => HostAppearanceOverride::Light,
                "dark" => HostAppearanceOverride::Dark,
                _ => HostAppearanceOverride::Auto,
            };
            assert_eq!(appearance.as_str(), value);
        }
    }
}
