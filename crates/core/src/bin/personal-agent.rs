fn main() {
    let command = std::env::args().nth(1).unwrap_or_else(|| "status".into());
    match command.as_str() {
        "status" | "doctor" => println!(
            "{}",
            serde_json::to_string_pretty(&personal_agent_core::diagnostic_snapshot())
                .expect("diagnostics")
        ),
        _ => {
            eprintln!("usage: personal-agent [status|doctor]");
            std::process::exit(2);
        }
    }
}
