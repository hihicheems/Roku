use roku_cmd::run_once;
use roku_common_types::ResponseStatus;

#[test]
fn e2e_happy_path_succeeds() {
	let response = run_once("build execution graph").expect("pipeline should run");
	assert!(matches!(response.status, ResponseStatus::Succeeded));
}
