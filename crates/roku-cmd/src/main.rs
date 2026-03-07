fn main() {
	match roku_cmd::execute_cli(std::env::args().skip(1)) {
		Ok(Some(output)) => println!("{output}"),
		Ok(None) => {}
		Err(error) => {
			eprintln!("error: {error}");
			std::process::exit(1);
		}
	}
}
