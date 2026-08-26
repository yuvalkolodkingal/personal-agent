fn main() {
    let response = personal_agent_core::run_cli(std::env::args_os().skip(1));
    if !response.stdout.is_empty() {
        print!("{}", response.stdout);
    }
    if !response.stderr.is_empty() {
        eprint!("{}", response.stderr);
    }
    if response.exit_code != 0 {
        std::process::exit(response.exit_code);
    }
}
