fn main() {
	match roku_cmd::run_once("bootstrap request") {
		Ok(response) => println!("{}", response.message),
		Err(error) => eprintln!("error: {}", error),
	}
}
