fn main() {
    eprintln!("jarvisctl is the legacy compatibility shim; prefer `personal-agent`.");
    let command = std::env::args().nth(1).unwrap_or_else(|| "status".into());
    match command.as_str() {
        "status" | "doctor" => println!(
            "{}",
            serde_json::to_string_pretty(&personal_agent_core::diagnostic_snapshot())
                .expect("diagnostics")
        ),
        _ => {
            eprintln!("this command is not yet available in the compatibility shim");
            std::process::exit(2);
        }
    }
}
