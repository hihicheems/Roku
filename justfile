# List available just recipes
@list:
    # This command lists all available just recipes in the current justfile.
    just --list

# Format the codebase
@fmt:
    # This command formats all Rust code using the stable rustfmt.
    cargo fmt --all
    # This command formats all files according to the hawkeye configuration.
    hawkeye format

# Alias for linting
alias l := lint

# Lint the codebase
@lint:
    # This command checks the codebase for issues according to the hawkeye configuration.
    hawkeye check
    # This command checks the Rust codebase for errors and warnings.
    cargo check --locked --all --all-features --all-targets
    # This command runs clippy to lint the Rust codebase and treats warnings as errors.
    cargo clippy --locked --all-targets --workspace -- -D warnings

# Calculate lines of code
@cloc:
    # This command calculates the lines of code in the project, excluding specified directories.
    cloc . --exclude-dir=vendor,tests,examples,build,target,.roku

# Alias for testing
alias t := test

# Run tests
@test:
    # This command runs all tests in the workspace using nextest.
    cargo nextest run --locked --workspace

# Build a Docker image for the current local architecture.
@docker-build image="roku:local":
    ./scripts/docker-build-current.sh {{image}}

# Validate a multi-architecture Docker build without pushing.
@docker-build-multiarch image="roku:latest":
    ./scripts/docker-build-multiarch.sh {{image}}

# Build and push a multi-architecture Docker image.
@docker-build-multiarch-push image:
    ./scripts/docker-build-multiarch.sh {{image}} --push

# Start all registered long-running dev services
@start-all:
    ./scripts/dev-services.sh start-all

# Stop all registered long-running dev services
@stop-all:
    ./scripts/dev-services.sh stop-all

# Show current dev service health in a table
@doctor:
    ./scripts/dev-services.sh doctor

# Start one named dev service
@start service="telegram-bot":
    ./scripts/dev-services.sh start {{service}}

# Stop one named dev service
@stop service="telegram-bot":
    ./scripts/dev-services.sh stop {{service}}

# Restart one named dev service
@restart service="telegram-bot":
    ./scripts/dev-services.sh stop {{service}}
    ./scripts/dev-services.sh start {{service}}

# Show one named dev service status
@status service="telegram-bot":
    ./scripts/dev-services.sh status {{service}}

# Start the external OpenViking provider used by live Roku memory/runtime flows.
@openviking-start:
    ./scripts/dev-openviking.sh start

# Stop the background OpenViking provider started by `just openviking-start`.
@openviking-stop:
    ./scripts/dev-openviking.sh stop

# Restart the background OpenViking provider with the same health-gated flow.
@openviking-restart:
    ./scripts/dev-openviking.sh restart

# Run the repo-local Ralph outer loop using Codex CLI.
@ralph iterations="10" state_dir=".ralph":
    ./scripts/ralph/ralph.sh --state-dir {{state_dir}} {{iterations}}
