# Roku Agent Dev Log

## 2026-03-07 - Session Milestone

### Completed Modules

- Workspace renamed to `roku-xxx` crate naming.
- Core architecture crates scaffolded and implemented with compile-safe interfaces.
- Planning chain implemented:
  - `roku-planning-engine`
  - `roku-task-planner`
  - `roku-execution-graph-builder`
- Runtime execution chain implemented:
  - `roku-agent-instance-factory`
  - `roku-agent-runtime`
  - `roku-tool-runtime`
- Security and correctness controls implemented:
  - `roku-capability-auth`
  - `roku-validation-plane`
- Integration boundaries implemented:
  - `roku-mcp-bridge`
  - `roku-coding-provider-adapter`
  - `roku-api-gateway`
  - `roku-connectors-telegram`
  - `roku-state-store`
  - `roku-observability`
- `roku-cmd` provides a minimal end-to-end pipeline from request normalization to validated response.
- `roku-e2e` adds end-to-end smoke test coverage.

### Verification Status

- `cargo fmt --all`: passed
- `cargo check --workspace`: passed
- `cargo test --workspace`: passed
- `cargo clippy --workspace --all-targets -- -D warnings`: passed

### Remaining Work

- Add production-grade transport and persistence adapters.
- Deepen validation policies and semantic checker coverage.
- Add failure-path e2e scenarios and approval gate test matrix.
- Implement richer observability export adapters.

### Next Recommended Steps

1. Add persistent task and event repositories with trait-backed storage adapters.
2. Introduce HTTP server integration in `roku-api-gateway`.
3. Add integration tests for retry/dead-letter and capability attenuation edge cases.

## 2026-03-07 - Session Milestone (Phase 2)

### Completed Modules

- `roku-state-store`
  - Refactored to trait-backed repositories.
  - Added `InMemoryTaskRepository` and `InMemoryEventRepository`.
  - Added file-backed adapters: `FileTaskRepository` and `FileEventRepository`.
  - Added roundtrip tests for in-memory and file adapters.
- `roku-api-gateway`
  - Added `actix-web` HTTP integration.
  - Added `/health` and `/v1/requests` routes.
  - Added `GatewayAppState` and `RequestExecutor` boundary.
  - Added route tests with `actix_web::test`.
- `roku-validation-plane`
  - Added staged checks: schema, semantic, provenance, policy.
  - Added schema-specific semantic validation for `backtest_report.v1` payload fields.
  - Added validation tests for pass and fail scenarios.
- `roku-observability`
  - Added `MetricsSnapshot` and validation failure counter.
  - Added `AuditSink` abstraction.
  - Added `InMemoryAuditSink` and `JsonlAuditExporter`.
  - Added exporter and metrics tests.
- `roku-cmd`
  - Added `RunMode` support: `Normal`, `MissingEvidence`, `CapabilityDenied`.
  - Integrated capability verification and audit recording in pipeline.
  - Added failure-mode tests.
- `roku-e2e`
  - Added failure-path e2e coverage for validation failure and capability denied.

### Verification Status

- `cargo fmt --all`: passed
- `cargo check --workspace`: passed
- `cargo test --workspace`: passed
- `cargo clippy --workspace --all-targets -- -D warnings`: passed

### Remaining Work

- Add production-grade PostgreSQL/Redis/NATS adapters behind repository traits.
- Wire real orchestrator dispatch into HTTP gateway executor.
- Expand approval-gate and dead-letter behavior into integration/e2e scenarios.
- Add observability sink integration in runtime services.

### Next Recommended Steps

1. Add `StorageBackend` trait with dedicated PG/Redis/NATS modules and integration tests.
2. Introduce `roku-runtime-service` crate to host orchestration API and gateway executor binding.
3. Add e2e matrix for retry budget exhaustion and dead-letter transitions.

## 2026-03-07 - Session Milestone (Phase 3)

### Completed Modules

- `roku-runtime-service`
  - Added reusable orchestration service boundary to host planning, graph compilation, execution, validation, state persistence, and audit hooks.
  - Moved the main execution flow out of `roku-cmd` so CLI and HTTP can reuse the same runtime entrypoint.
  - Upgraded execution from "first graph node only" to sequential graph-node processing.
  - Added explicit run modes for approval gating and retry-budget exhaustion.
- `roku-api-gateway`
  - Added `RuntimeServiceExecutor` adapter to bind HTTP ingress directly to the real runtime service.
  - Preserved generic `RequestExecutor` abstraction while replacing the placeholder-only path for integration use.
- `roku-common-types`
  - Added `ResponseStatus::PendingApproval` to represent approval gates as a first-class response outcome.
- `roku-orchestrator`
  - Added `register_failure` helper to emit `Failed` and `DeadLetter` events coherently.
  - Added explicit `Failed -> Planning` retry path in the state machine for replan/retry flows.
  - Added tests for retry-budget exhaustion and dead-letter transitions.
- `roku-e2e`
  - Added HTTP integration coverage against the real runtime service.
  - Added end-to-end approval-gate and dead-letter scenarios.

### Verification Status

- `cargo fmt --all`: passed
- `cargo test --workspace`: passed

### Remaining Work

- Add resumable approval handling so a `WaitingApproval` task can continue from persisted state instead of only returning a pending response.
- Add production storage and queue backends behind repository abstractions.
- Deepen graph execution to support branching, joins, and partial re-run from checkpoints.
- Expand validation to schema registry integration, provenance cross-checks, and domain-specific semantic policies.

### Next Recommended Steps

1. Introduce persisted approval tickets and a resume API for `WaitingApproval -> Executing`.
2. Add PostgreSQL-backed task/event repositories and a Redis/NATS-backed dispatch layer.
3. Extend `ExecutionGraphBuilder` and `roku-runtime-service` from linear node iteration to dependency-aware DAG scheduling.

## 2026-03-07 - Session Milestone (Phase 4)

### Completed Modules

- `roku-common-types`
  - Added approval contracts: `ApprovalId`, `ApprovalStatus`, `ApprovalDecision`, `ApprovalTicket`.
  - Added task checkpoint fields: `next_node_index`, `pending_approval_id`, `last_result`.
- `roku-state-store`
  - Added `ApprovalRepository` abstraction.
  - Added `InMemoryApprovalRepository` and `FileApprovalRepository`.
  - Extended repository tests to cover approval ticket roundtrip.
- `roku-runtime-service`
  - Persisted approval tickets when execution reaches `WaitingApproval`.
  - Added resumable execution from persisted task checkpoint and approval state.
  - Added `get_approval` and `decide_approval` service APIs.
  - Added runtime tests for approval grant, rejection, and duplicate decision protection.
- `roku-api-gateway`
  - Added `GET /v1/approvals/{approval_id}`.
  - Added `POST /v1/approvals/{approval_id}/decision`.
  - Extended gateway executor boundary from request-only to request + approval operations.
- `roku-e2e`
  - Added full HTTP approval roundtrip coverage: submit -> pending approval -> query -> approve -> succeeded.

### Verification Status

- `cargo fmt --all`: passed
- `cargo test --workspace`: passed

### Remaining Work

- Add explicit approval rejection policy options (`Failed` vs `Cancelled`) and organization-level approval rules.
- Add DAG-aware scheduling instead of current linear graph iteration.
- Add PostgreSQL/Redis/NATS production backends behind repository and dispatch abstractions.
- Add artifact store and experiment registry instead of keeping recovery context only in task state.
- Add richer capability attenuation rules tied to approval-sensitive actions.

### Next Recommended Steps

1. Introduce dependency-aware node scheduling and checkpoint recovery beyond linear graphs.
2. Add `StorageBackend`-style production adapters for PostgreSQL task/event state and Redis/NATS dispatch.
3. Start implementing `Artifact Store` and `Experiment Registry` so validation and recovery stop depending on in-task ephemeral result snapshots.

## 2026-03-07 - Session Milestone (Phase 5)

### Completed Modules

- `roku-execution-graph-builder`
  - Split the crate into dedicated modules instead of a single `lib.rs`.
  - Added `TaskGraphScheduler` with ready-node selection, execution layering, completion checks, and cycle detection.
  - Added scheduler tests for linear flow, parallel branches, and invalid cyclic graphs.
- `roku-common-types`
  - Added `completed_nodes` to persisted task checkpoint state for DAG-aware resume.
- `roku-runtime-service`
  - Switched task progression from linear index scanning to scheduler-driven execution based on completed node state.
  - Kept approval resume compatible with the new checkpoint model.
- `roku-api-gateway`
  - Split the crate into `executor`, `models`, and `routes` modules instead of keeping all HTTP logic in a single `lib.rs`.
  - Preserved HTTP and approval behavior while improving module boundaries.
- `roku-observability`
  - Added metrics for approval creation, approval resolution, and dead-letter outcomes.

### Verification Status

- `cargo fmt --all`: passed
- `cargo test -p roku-execution-graph-builder`: passed
- `cargo test -p roku-runtime-service -p roku-e2e`: passed
- `cargo test -p roku-api-gateway -p roku-e2e`: passed
- `cargo test -p roku-observability -p roku-runtime-service -p roku-e2e`: passed

### Remaining Work

- Replace the current single `last_result` checkpoint with node-scoped result storage so validation and aggregation work correctly for true multi-branch DAGs.
- Split `roku-runtime-service` into multiple files; it is still too large for the responsibility it now carries.
- Extend `ExecutionGraphBuilder` from linear compilation to dependency-aware plan compilation with explicit recovery metadata.
- Add production storage and queue backends behind repository abstractions.
- Start the `Artifact Store` and `Experiment Registry` implementation.

### Next Recommended Steps

1. Add node-scoped result checkpoint storage and aggregation semantics for multi-branch DAG execution.
2. Refactor `roku-runtime-service` into modules (`service`, `execution`, `approval`, `tests`) without changing behavior.
3. Start implementing artifact and experiment persistence so the runtime no longer relies on task-local result snapshots alone.

## 2026-03-07 - Session Milestone (Phase 6)

### Completed Modules

- `roku-state-store`
  - Added `ResultRepository` abstraction for node-scoped execution results.
  - Added `InMemoryResultRepository` and `FileResultRepository`.
  - Extended repository tests to cover persisted results.
- `roku-runtime-service`
  - Switched validation input lookup from single `last_result` fallback to repository-backed upstream result traversal.
  - Added a unit test to ensure validation can resolve execution evidence through approval nodes.
  - Split helper functions and tests out of the main `lib.rs` into dedicated module files.
- `roku-e2e`
  - Re-ran approval and failure-path e2e coverage against the new result repository integration.

### Verification Status

- `cargo fmt --all`: passed
- `cargo test -p roku-state-store -p roku-runtime-service`: passed
- `cargo test -p roku-runtime-service -p roku-e2e`: passed

### Remaining Work

- Replace the current ad-hoc upstream result traversal with explicit node result/aggregation semantics for true multi-parent validation nodes.
- Continue splitting `roku-runtime-service`; the main service file is still too large and mixes construction, orchestration, and execution details.
- Add artifact and experiment persistence so node-scoped results can graduate from transient runtime storage to formal data-plane objects.
- Add production persistence and queue backends.

### Next Recommended Steps

1. Introduce explicit node result aggregation types instead of relying on raw `ResultEnvelope` lists for validation joins.
2. Continue splitting `roku-runtime-service` into `service`, `execution`, and `approval` modules.
3. Start the `Artifact Store` / `Experiment Registry` implementation and route validation evidence through that layer.

## 2026-03-07 - Session Milestone (Phase 7)

### Completed Modules

- `roku-common-types`
  - Added `Artifact`, `ArtifactId`, `ExperimentRun`, `ExperimentMetric`, and `ValidationEvidenceSet`.
- `roku-artifact-store`
  - Added dedicated data-plane crate with in-memory and file-backed artifact repositories.
  - Added `ArtifactStore` service to persist node result artifacts and resolve them by URI.
- `roku-experiment-registry`
  - Added dedicated data-plane crate with in-memory and file-backed experiment run repositories.
  - Added `ExperimentRegistry` service for start/attach/complete/fail lifecycle management.
- `roku-observability`
  - Added artifact and experiment metrics counters.
- `roku-validation-plane`
  - Added artifact-backed provenance validation via `ValidationEvidenceSet`.
  - Added tests for missing artifact evidence and valid artifact-backed payloads.
- `roku-runtime-service`
  - Persisted execution results as formal artifacts.
  - Started and finalized experiment runs around task execution.
  - Attached persisted artifacts to experiment runs.
  - Switched validation input construction from raw results to artifact-backed evidence sets.

### Verification Status

- `cargo test -p roku-common-types -p roku-artifact-store -p roku-experiment-registry`: passed
- `cargo test -p roku-observability -p roku-validation-plane -p roku-runtime-service -p roku-e2e`: passed

### Remaining Work

- `ExecutionGraphBuilder` still compiles a linear outline rather than a true dependency-aware multi-parent DAG with explicit join policies.
- Validation still aggregates by iterating raw evidence sets; explicit `AggregationPolicy` / `NodeResultSet` types are not implemented yet.
- Artifact and experiment state are in-memory/file-backed only; PostgreSQL/Redis/NATS backends remain missing.
- Queue/dispatch plane and organization-layer A2A contracts are still not implemented.

### Next Recommended Steps

1. Introduce explicit aggregation contracts for multi-parent validation and aggregation nodes.
2. Extend `ExecutionGraphBuilder` so plan compilation can emit real branch/join graphs instead of only linear flows.
3. Add production backends for artifact, experiment, task, and event persistence.

## 2026-03-07 - Session Milestone (Phase 8)

### Completed Modules

- `roku-runtime-service`
  - Split the crate into `data_plane` and `execution` modules, reducing root-file responsibility.
- `roku-api-gateway`
  - Added task data endpoints:
    - `GET /v1/tasks/{task_id}/artifacts`
    - `GET /v1/tasks/{task_id}/experiment`
  - Extended gateway executor traits to expose artifact and experiment queries.
  - Added route tests for task data lookup behavior.
- `roku-e2e`
  - Added HTTP integration coverage for task artifact listing and experiment lookup after request execution.

### Verification Status

- `cargo test -p roku-runtime-service -p roku-e2e`: passed
- `cargo test -p roku-api-gateway -p roku-e2e`: passed

### Remaining Work

- Runtime root file is smaller, but approval flow still shares the main entry module and can be extracted further.
- HTTP layer exposes read-only task data; no artifact download/content endpoint exists yet.
- Data plane is still prototype-grade and lacks retention, versioning, and backend abstraction parity with the design doc.

### Next Recommended Steps

1. Introduce explicit artifact content retrieval / download endpoints once artifact payload storage is separated from metadata.
2. Finish splitting `roku-runtime-service` so approval handling and orchestration entrypoints are isolated.
3. Push `ExecutionGraphBuilder` and runtime scheduling toward true graph joins and resumable partial reruns.

## 2026-03-07 - Session Milestone (Phase 9)

### Completed Modules

- `roku-common-types`
  - Added explicit graph-join contracts: `JoinPolicy`, `AggregationMode`, `NodeResultSet`.
- `roku-runtime-service`
  - Added `collect_node_result_set` to replace implicit "grab all upstream results" behavior with a typed result-set contract.
  - Added branch resolution logic that walks through approval or non-result nodes until result-producing ancestors are found.
  - Added join-policy enforcement for `AllParents`, `AnyParent`, and `Quorum`.
  - Added aggregation-mode support for `CollectAll` and `HighestConfidence`.
  - Switched validation evidence collection to be built from `NodeResultSet`.
  - Added aggregation-node processing path that validates upstream branch availability before marking the node complete.
- `roku-runtime-service` tests
  - Added coverage for highest-confidence aggregation and quorum enforcement.

### Verification Status

- `cargo test -p roku-common-types -p roku-execution-graph-builder -p roku-runtime-service`: passed

### Remaining Work

- `ExecutionGraphBuilder` still emits a mostly linear graph; join policy now exists in types and runtime, but planner/builder do not yet populate richer dependency metadata.
- Aggregation nodes validate branch readiness, but they do not yet emit dedicated aggregation artifacts or summaries.
- Validation and aggregation still share the same result-set primitives; no specialized aggregation artifact schema exists yet.

### Next Recommended Steps

1. Extend `PlanStep` and `ExecutionGraphBuilder` so dependency sketches from the planner become real branch/join graphs.
2. Emit aggregation-specific artifacts or summaries for aggregation nodes instead of only marking the node complete.
3. Revisit scheduler and retry behavior once multi-parent graphs are emitted by the builder.

## 2026-03-07 - Session Milestone (Phase 10)

### Completed Modules

- `roku-common-types`
  - Added `depends_on` to `PlanStep`, allowing the planner to emit dependency sketches instead of only implicit step order.
- `roku-task-planner`
  - Updated the default planner to output explicit linear dependency metadata.
- `roku-execution-graph-builder`
  - Reworked `compile` into a fallible graph compiler with `GraphBuildError`.
  - Added duplicate-step and missing-dependency detection.
  - Switched graph compilation from "previous node -> next node" to dependency-driven edge generation.
  - Changed validation-gate wiring to depend on terminal steps instead of only the last step, enabling branch/join style graphs.
  - Added builder tests for branch validation joins and missing dependency rejection.
- `roku-runtime-service`
  - Added graph-build failure handling so invalid plan outlines return structured failed responses instead of silently building broken graphs.

### Verification Status

- `cargo test -p roku-task-planner -p roku-execution-graph-builder -p roku-runtime-service`: passed

### Remaining Work

- Task planner still emits only a trivial two-step outline; it now supports dependency metadata, but it does not yet synthesize richer branch plans from planning mode.
- Validation join behavior is now supported by the builder, but no planner strategy currently emits multi-branch outlines in production flow.
- Graph compilation still does not inject recovery metadata or explicit partial rerun markers.

### Next Recommended Steps

1. Teach the planner to emit branch candidates and dependency sketches for non-trivial modes such as `TaskDecomposition` or `TreeSearch`.
2. Add aggregation-node artifact generation now that join semantics and branch compilation exist.
3. Extend graph compilation with recovery metadata and partial rerun anchors.

## 2026-03-07 - Session Milestone (Phase 11)

### Completed Modules

- `roku-connectors-telegram`
  - Split the connector into `inbound` and `outbound` modules.
  - Added Telegram webhook/update models with serde support.
  - Added inbound normalization with explicit error handling for missing message, missing text, and bot-originated messages.
  - Added outbound message formatting that maps runtime responses into Telegram-ready text payloads.
  - Added connector unit tests for inbound mapping, bot-message rejection, and outbound formatting.

### Verification Status

- `cargo test -p roku-connectors-telegram`: passed

### Remaining Work

- The Telegram connector still does not include actual long-polling or webhook transport integration.
- No Telegram-specific approval interaction or callback-query workflow exists yet.
- Connector output is plain text formatting only; no keyboard or richer interaction model is implemented.

### Next Recommended Steps

1. Add callback-query and approval decision mapping so Telegram can drive approval workflows directly.
2. Introduce a thin transport layer for webhook or polling execution outside the pure adapter crate.
3. Reuse task artifact / experiment query endpoints to build richer Telegram responses for long-running tasks.

## 2026-03-07 - Session Milestone (Phase 12)

### Completed Modules

- `roku-planning-engine`
  - Added planning loop controls with explicit `PlanningLoopState` and `PlanningStopReason`.
  - Extended strategy decision output with `max_branches` and mode-specific `PlanningHook` sets.
  - Added stop-condition evaluation API to avoid unbounded planner loops.
- `roku-task-planner`
  - Switched planner interface to consume full `PlanningDecision` rather than only `PlanningMode`.
  - Added mode-aware outline generation for `ReAct` / `TaskDecomposition` / `TreeSearch` / `IterativeRefinement`.
  - Added dependency-aware branch merge and critique-loop step generation.
- `roku-runtime-service`
  - Updated runtime planning flow to pass strategy decisions directly into the task planner.

### Verification Status

- `cargo test -p roku-planning-engine -p roku-task-planner -p roku-runtime-service`: passed

### Remaining Work

- Planning loops are now explicit, but no runtime feedback channel from execution metrics into planner stop conditions exists yet.
- `TaskPlanner` can emit richer branch outlines, but no heuristic score is applied yet when selecting among tree-search branches.
- Tool runtime still lacks production-level descriptor/runtime constraints and deterministic execution hooks.

### Next Recommended Steps

1. Implement `roku-tool-runtime` descriptor contracts with sandbox profile, timeout, retry, and deterministic hook APIs.
2. Feed runtime execution telemetry into planning feedback so replan and stop decisions can use observed failure patterns.
3. Add planner quality metrics (branch acceptance rate, critique-loop convergence) into observability exports.

## 2026-03-07 - Session Milestone (Phase 13)

### Completed Modules

- `roku-tool-runtime`
  - Replaced string-only echo runtime with descriptor-based tool registry.
  - Added `ToolDescriptor`, `ToolSchema`, `RuntimeConstraints`, and `SandboxProfile` contracts.
  - Added execution policy enforcement:
    - required capability checks
    - required input-field schema checks
    - timeout guard
    - bounded retry with optional backoff
  - Added deterministic execution hook stream:
    - `ExecutionEventKind` lifecycle events
    - stable `trace_id` generation (`invocation_key:attempt:event`)
    - optional output fingerprint emission for deterministic tracing
  - Added structured error model with explicit failure classes (`ToolNotFound`, `CapabilityDenied`, `Timeout`, `ExecutionFailed`).
  - Added unit tests for success path, capability denial, retriable failure recovery, timeout behavior, and deterministic hook ordering.

### Verification Status

- `cargo test -p roku-tool-runtime`: passed
- `cargo fmt --all`: passed
- `cargo check --workspace`: passed

### Remaining Work

- Runtime service still executes a generic worker path and has not yet routed node execution through `roku-tool-runtime`.
- Descriptor input checking currently validates required fields only; full schema registry integration is still missing.
- Sandbox profile is declared and enforced at policy level, but actual OS/container isolation adapters are not yet wired.

### Next Recommended Steps

1. Integrate `roku-tool-runtime` into `roku-agent-runtime` / `roku-runtime-service` node execution flow.
2. Extend schema checks from required fields to versioned schema validation contracts.
3. Add adapter layer for real sandbox executors (WASI/container) behind the current runtime constraints.

## 2026-03-07 - Session Milestone (Phase 14)

### Completed Modules

- `roku-agent-instance-factory`
  - Upgraded from fixed role wiring to capability profile assembly.
  - Added `CapabilityProfile` registry with default profiles (`research` / `data` / `review` / `general`) and dynamic profile registration.
  - Added profile inference by capability prefixes and fallback profile strategy.
  - Added merged capability set generation (`profile defaults + node-required capabilities`).
  - Added policy binding derivation by profile baseline and node capability complexity.
- `roku-agent-runtime`
  - Upgraded runtime into capability-dispatched worker registry.
  - Added built-in workers (`research-worker`, `data-worker`, `review-worker`, `generic-worker`) with priority-based dispatch.
  - Added policy guardrails: reject execution when budget/time policy is exhausted.
  - Added runtime extension API to register custom workers dynamically.
- `roku-runtime-service`
  - Updated construction path to use the new non-unit `AgentInstanceFactory` / `GenericAgentRuntime` default initialization.

### Verification Status

- `cargo test -p roku-agent-instance-factory -p roku-agent-runtime -p roku-runtime-service`: passed
- `cargo fmt --all`: passed
- `cargo check --workspace`: passed

### Remaining Work

- Runtime dispatch currently uses capability prefixes only; no workload-aware or cost-aware worker routing model exists yet.
- Profile inference does not yet consume historical success/failure signals or policy feedback loops.
- Agent runtime still uses synthetic worker outputs and has not been wired to tool-runtime descriptors for real tool execution contracts.

### Next Recommended Steps

1. Wire `roku-agent-runtime` worker execution to `roku-tool-runtime` so profile-dispatched workers run descriptor-governed tools.
2. Add profile routing feedback loop using observability metrics (success rate, validation failures, timeout distribution).
3. Introduce profile-level approval/capability attenuation templates to tighten high-risk worker operations.

## 2026-03-07 - Session Milestone (Phase 15)

### Completed Modules

- `roku-observability`
  - Added planning metrics family:
    - `planning_runs_total`
    - per-strategy counters (`react`, `task_decomposition`, `tree_search`, `iterative_refinement`)
  - Added normalized planning-mode classifier for strategy metric accounting.
  - Extended audit model with correlation and attributes:
    - `AuditCorrelation` (`trace_id`, `span_id`, optional `task_id`/`request_id`)
    - `AuditAttribute` key-value tags
    - `AuditRecord` builder APIs (`new`, `with_correlation`, `with_attribute`)
  - Kept JSONL exporter compatibility while serializing enriched audit records.
- `roku-runtime-service`
  - Integrated planning metrics updates in execute flow.
  - Added correlated audit records for:
    - capability denial
    - validation acceptance
    - approval decision
  - Added test coverage for planning metric increment behavior.

### Verification Status

- `cargo test -p roku-observability -p roku-runtime-service`: passed
- `cargo fmt --all`: passed
- `cargo check --workspace`: passed

### Remaining Work

- Correlation currently uses deterministic local trace IDs derived from request ID; full distributed trace propagation is not connected yet.
- Observability crate still exports raw snapshots only; no Prometheus/OpenTelemetry bridge implementation is present.
- Planning metrics capture strategy selection but not loop convergence quality (iteration depth, stop reasons).

### Next Recommended Steps

1. Add runtime propagation of trace context from gateway ingress to all audit and tool execution events.
2. Add OTEL/Prometheus exporters on top of current in-memory counters.
3. Track planning stop reasons and loop depth histograms for strategy quality diagnosis.

## 2026-03-07 - Session Milestone (Phase 16)

### Completed Modules

- `roku-artifact-store`
  - Added artifact content persistence APIs in repository contracts (`save_content`, `load_content_by_uri`).
  - Added content storage for both in-memory and file-backed repositories.
  - Added backward-compatible file snapshot format upgrade to carry both metadata and content.
  - Persisted node result payload as artifact content during artifact creation.
- `roku-runtime-service`
  - Added `get_artifact_content(task_id, artifact_id)` data-plane API.
  - Added cross-task protection: reject artifact content access when artifact does not belong to task.
  - Added tests for artifact content retrieval and cross-task rejection.
- `roku-api-gateway`
  - Added new artifact data endpoints:
    - `GET /v1/tasks/{task_id}/artifacts/{artifact_id}/content`
    - `GET /v1/tasks/{task_id}/artifacts/{artifact_id}/download`
  - Added finer-grained artifact error mapping (`404` not found, `403` task mismatch, `400` fallback).
  - Added response model `ArtifactContentResponse`.
- `roku-e2e`
  - Extended HTTP e2e flow to validate artifact content and download endpoints end-to-end.

### Verification Status

- `cargo test -p roku-artifact-store -p roku-runtime-service -p roku-api-gateway -p roku-e2e`: passed
- `cargo fmt --all`: passed
- `cargo check --workspace`: passed

### Remaining Work

- Artifact content is currently plain string payload storage; no binary/blob abstraction or media-type catalog exists yet.
- Download endpoint serves text/plain only; no signed URL / streaming / range support.
- Artifact retention and content encryption policies are not yet implemented.

### Next Recommended Steps

1. Introduce artifact blob abstraction (`bytes + media_type + size`) and object-store adapters.
2. Add content retention and lifecycle policies (TTL, archival, GC).
3. Add authenticated download policies and optional signed URL mode for external clients.

## 2026-03-07 - Session Milestone (Phase 17)

### Completed Modules

- `roku-llm-adapter`
  - Added new crate for multi-provider model routing.
  - Introduced risk-aware model contract:
    - `RiskTier`
    - `ModelProfile` with `max_risk_tier`, context window, and cost metadata
  - Introduced budget-aware request contract:
    - token budget guard
    - cost budget guard
    - preferred provider constraint
  - Added routing policy contract (`max_request_cost_usd`, `max_latency_ms`).
  - Added provider abstraction (`LlmProvider`) and response contracts (`ProviderResponse`, `LlmResponse`).
  - Implemented `LlmRouter`:
    - model eligibility filtering
    - high-risk preference for stronger models
    - low-risk preference for lower-cost models
    - provider registration and dispatch
    - post-call budget/latency enforcement
  - Added error taxonomy (`NoEligibleModel`, `ProviderNotRegistered`, `BudgetExceeded`, `LatencyExceeded`, `ProviderCallFailed`).
  - Added unit tests for strategy routing, preferred provider, budget rejection, and latency rejection.

### Verification Status

- `cargo test -p roku-llm-adapter`: passed
- `cargo fmt --all`: passed
- `cargo check --workspace`: passed

### Remaining Work

- `roku-llm-adapter` is not yet wired into planning/runtime execution flow.
- No provider-specific retry/circuit-breaker policy is attached yet.
- No request/response audit hook integration into observability layer yet.

### Next Recommended Steps

1. Integrate `roku-llm-adapter` into planning/reasoning calls in `roku-runtime-service`.
2. Add provider-level resilience policy (retry budget, jitter backoff, breaker state).
3. Emit model routing and cost telemetry through `roku-observability`.

## 2026-03-07 - Session Milestone (Phase 18)

### Completed Modules

- `docs/todo-list.md`
  - Reworked from a flat milestone table into a crate-oriented implementation roadmap.
  - Reorganized the roadmap in the same layer order as `tmp/agent-design-doc.md`.
  - Split work into crate-level sections with fine-grained subtasks and explicit `DONE` / `TODO` state.
  - Added planned-crate sections for architecture gaps already required by the design doc:
    - `roku-supervisor-agent`
    - `roku-skill-registry`
    - `roku-agent-directory`
    - `roku-quant-domain-pack`
  - Turned the roadmap into the primary execution reference for future development sessions.

### Verification Status

- Manual consistency review against `tmp/agent-design-doc.md`: completed
- Manual consistency review against implemented crates and `docs/dev-log.md`: completed

### Remaining Work

- The roadmap is now significantly more useful, but it will need ongoing maintenance as crates split or new planned crates are actually scaffolded.
- Some future tasks are still intentionally grouped under current crates (`roku-runtime-service`, `roku-tool-runtime`, `roku-state-store`) until the corresponding dedicated crates are extracted.

### Next Recommended Steps

1. Start executing the new roadmap from the highest-value unfinished infrastructure items in `roku-state-store`.
2. Use the planned-crate sections as the trigger for when architecture pressure justifies extracting new crates from current integration crates.
3. Keep `docs/todo-list.md` synchronized whenever a task lands or a crate boundary changes.

## 2026-03-07 - Session Milestone (Phase 19)

### Completed Modules

- `roku-task-planner`
  - Replaced the prior placeholder-style planner name with `AdaptiveTaskPlanner`.
  - Split the crate into explicit modules:
    - `planner.rs`
    - `strategies.rs`
  - Kept the existing planning behaviors intact while cleaning crate boundaries.
- `roku-mcp-bridge`
  - Reworked the crate from a single placeholder file into explicit modules:
    - `bridge.rs`
    - `catalog.rs`
    - `error.rs`
    - `types.rs`
  - Added `McpToolDescriptor`, `McpToolCatalog`, and structured `McpError`.
  - Added `discover_tools` support and a registry-backed `InMemoryMcpBridge`.
  - Added tests for discovery, unknown-tool rejection, and deterministic tool-call response lookup.
- `roku-coding-provider-adapter`
  - Reworked the crate into explicit modules:
    - `contract.rs`
    - `error.rs`
    - `provider.rs`
  - Added `CodingProviderError` taxonomy.
  - Added strict `CodingWorkContract` validation before provider execution.
  - Switched MCP calls to send the full serialized work contract instead of only the goal string.
  - Added structured provider payload parsing and normalization into `CodeChangeReport`.
  - Rejected unstructured provider payloads instead of silently accepting free text.
  - Added tests for invalid contract rejection, structured execution success, and malformed provider response rejection.

### Verification Status

- `cargo test -p roku-task-planner -p roku-mcp-bridge -p roku-coding-provider-adapter -p roku-runtime-service -p roku-e2e`: passed
- `cargo fmt --all`: passed
- `cargo check --workspace`: passed
- `cargo clippy --workspace --all-targets -- -D warnings`: passed
- Placeholder-style naming scan across `crates/` and `docs/`: clean

### Remaining Work

- `roku-mcp-bridge` still lacks real transport/session recovery and external server integration.
- `roku-coding-provider-adapter` still needs degraded fallback mode for provider unavailability or budget failure.
- `roku-agent-runtime` and `roku-runtime-service` still do not execute real work through the coding provider path.

### Next Recommended Steps

1. Integrate `roku-agent-runtime` with `roku-tool-runtime` so node execution stops returning synthetic results.
2. Add provider degradation and approval-aware capability attenuation in `roku-coding-provider-adapter`.
3. Start splitting other single-file crates (`roku-capability-auth`, `roku-validation-plane`, `roku-llm-adapter`) along the same module-oriented standard.

## 2026-03-07 - Session Milestone (Phase 20)

### Completed Modules

- `roku-validation-plane`
  - Split the crate into explicit validation stages instead of keeping all logic in a single `lib.rs`:
    - `config.rs`
    - `schema.rs`
    - `semantic.rs`
    - `provenance.rs`
    - `policy.rs`
    - `cross_check.rs`
    - `pipeline.rs`
  - Kept the existing schema, semantic, provenance, and policy validation flow intact behind the new module boundaries.
  - Added a baseline cross-check stage for artifact-reference deduplication, artifact/result schema consistency, and suspicious confidence on error results.
  - Extended pipeline tests to cover cross-check rejection paths.
- Repository-wide naming hygiene
  - Removed the remaining placeholder-style naming references from project documentation.

### Verification Status

- `cargo fmt --all`: passed
- `cargo test -p roku-validation-plane -p roku-runtime-service -p roku-e2e`: passed
- `cargo check --workspace`: passed
- `cargo clippy -p roku-validation-plane --all-targets -- -D warnings`: passed
- `cargo clippy --workspace --all-targets -- -D warnings`: passed
- Placeholder-style naming scan across the repository: clean

### Remaining Work

- `roku-validation-plane` still lacks schema registry integration and field-level compatibility diagnostics.
- Independent verification, dual-run reconciliation, and escalation into alternate workers or reviewer agents are still pending.
- Validation failures are still reported as flat strings rather than structured reason codes.

### Next Recommended Steps

1. Deepen `roku-agent-runtime` so workers execute through `roku-tool-runtime` instead of returning synthetic output.
2. Add structured validation reason codes and schema-registry-driven compatibility checks.
3. Extend validation failure handling toward reviewer escalation and quarantined `untrusted result` states.

## 2026-03-07 - Session Milestone (Phase 21)

### Completed Modules

- `roku-agent-runtime`
  - Split the crate into explicit modules:
    - `runtime.rs`
    - `workers.rs`
    - `tools.rs`
    - `result.rs`
  - Replaced the built-in worker path that directly assembled synthetic `ResultEnvelope` values.
  - Added descriptor-governed built-in tools for research, data, review, and generic execution paths.
  - Routed the default worker registry through `roku-tool-runtime`, including capability checks, sandbox metadata, deterministic invocation keys, and output fingerprint propagation.
  - Normalized tool execution success and failure into structured `ResultEnvelope` payloads.
- `roku-runtime-service`
  - Added a regression test proving persisted execution results now carry tool-runtime evidence and JSON payloads.

### Verification Status

- `cargo fmt --all`: passed
- `cargo test -p roku-agent-runtime -p roku-runtime-service -p roku-e2e`: passed
- `cargo check --workspace`: passed
- `cargo clippy --workspace --all-targets -- -D warnings`: passed

### Remaining Work

- `roku-agent-runtime` still does not route higher-complexity reasoning workers through `roku-llm-adapter`.
- Custom runtime workers can still bypass `roku-tool-runtime` if they return envelopes directly; only the built-in worker path is now tool-governed.
- `roku-tool-runtime` still lacks output-schema enforcement, artifact persistence, and sandbox backends.

### Next Recommended Steps

1. Push `roku-agent-runtime` deeper into `roku-llm-adapter` and provider-routing policy for nontrivial reasoning nodes.
2. Add output-schema enforcement and artifact/audit persistence to `roku-tool-runtime`.
3. Start the next infrastructure increment in `roku-state-store` or dispatch/backpressure plane.

## 2026-03-07 - Session Milestone (Phase 22)

### Completed Modules

- `roku-tool-runtime`
  - Split the crate into explicit modules:
    - `descriptor.rs`
    - `error.rs`
    - `event.rs`
    - `runtime.rs`
    - `tests.rs`
  - Preserved descriptor validation, capability checks, retry/timeout handling, deterministic hook emission, and fingerprint generation while removing the single-file bottleneck.
  - Kept the public API stable so `roku-agent-runtime` and existing tests continued to work without integration changes.

### Verification Status

- `cargo fmt --all`: passed
- `cargo test -p roku-tool-runtime -p roku-agent-runtime -p roku-runtime-service -p roku-e2e`: passed
- `cargo check --workspace`: passed
- `cargo clippy --workspace --all-targets -- -D warnings`: passed

### Remaining Work

- `roku-tool-runtime` still lacks output-schema enforcement against descriptor-declared schemas.
- Tool execution metadata is not yet persisted into artifacts or audit/event repositories.
- Sandbox backends remain logical profiles only; there is still no WASI/container execution adapter behind them.

### Next Recommended Steps

1. Implement `TR-08` so tool outputs are validated against declared output schemas before they re-enter agent execution.
2. Implement `TR-09` so tool execution produces auditable persisted artifacts and budget/accounting records.
3. Continue into `roku-state-store` production adapters or dispatch/backpressure infrastructure.

## 2026-03-07 - Session Milestone (Phase 23)

### Completed Modules

- `roku-llm-adapter`
  - Split the crate into explicit modules:
    - `types.rs`
    - `router.rs`
    - `openrouter.rs`
  - Added an `OpenRouterProvider` implementation over the existing router abstraction.
  - Added environment-backed OpenRouter bootstrap with support for account-default model fallback when `OPENROUTER_MODEL` is not set.
  - Added parser coverage for both string and part-array OpenRouter message content formats.
- `roku-agent-runtime`
  - Added `GenericAgentRuntime::with_llm_router(...)` so built-in workers can execute through live LLM-backed tool descriptors instead of only deterministic report tools.
  - Added a unit test that exercises the live LLM-backed runtime path without external network calls.
- `roku-task-planner`
  - Injected the original request goal into plan-step summaries so downstream execution workers receive the real user intent.
- `roku-runtime-service`
  - Successful responses now surface the last execution result message instead of the old fixed `"task succeeded"` placeholder.

### Verification Status

- `cargo fmt --all`: passed
- `cargo test -p roku-llm-adapter`: passed
- `cargo test -p roku-task-planner -p roku-agent-runtime -p roku-runtime-service -p roku-e2e`: passed
- `cargo check --workspace`: passed

### Remaining Work

- OpenRouter is integrated for runtime use, but there is still no provider retry/backoff/circuit-breaker policy.
- Planning and coding-provider routing are not yet using the llm-adapter path.
- Telegram transport is still missing a real polling or webhook runner.

### Next Recommended Steps

1. Add a real Telegram polling runner and wire it to the live runtime path.
2. Add `cargo clippy --workspace --all-targets -- -D warnings` after the Telegram integration lands.
3. Continue with tool output schema enforcement and artifact/audit persistence.

## 2026-03-07 - Session Milestone (Phase 24)

### Completed Modules

- `roku-connectors-telegram`
  - Added a real blocking Telegram Bot API client with environment-backed configuration.
  - Added a polling runner that converts inbound Telegram updates into `RequestEnvelope` values and sends formatted responses back to the originating chat.
  - Switched outbound formatting to plain text by default so LLM output does not break Telegram Markdown parsing.
- `roku-cmd`
  - Split the crate into explicit `runtime.rs` and `bot.rs` modules.
  - Added `live-once` and `telegram-bot` command paths.
  - Added environment-backed OpenRouter runtime bootstrap for live execution.
- `roku-runtime-service`
  - Execution-node failures now surface their real tool/provider error instead of being masked by downstream validation failure text.
- Live connectivity checks
  - `cargo run -p roku-cmd -- live-once 'Reply with the single word OK.'` succeeded with the provided OpenRouter key and returned `OK`.
  - Telegram `getMe` succeeded with the provided bot token; the bot identity resolved as `kikitest1024_bot`.
- `roku-llm-adapter`
  - Switched the default OpenRouter model fallback to the official free router `openrouter/free`, matching the current zero-cost bootstrap goal.

### Verification Status

- `cargo fmt --all`: passed
- `cargo test -p roku-connectors-telegram -p roku-cmd -p roku-e2e`: passed
- `cargo test -p roku-runtime-service`: passed
- `cargo test -p roku-llm-adapter -p roku-cmd`: passed
- `cargo clippy --workspace --all-targets -- -D warnings`: pending final rerun after docs sync

### Remaining Work

- Telegram polling is implemented, but long-task status push / approval callback interactions are still not implemented.
- Planning still uses hard-coded heuristic inputs; it is not yet driven by live llm-adapter requests.
- Tool output schema enforcement and artifact/audit persistence still need to be added on the tool-runtime side.

### Next Recommended Steps

1. Add `TR-08` and `TR-09` so live tool execution has output-schema validation and persisted audit/artifact records.
2. Add Telegram approval callbacks and long-task status updates.
3. Move planning inputs and richer reasoning paths onto the llm-adapter layer.

## 2026-03-07 - Session Milestone (Phase 25)

### Completed Modules

- `roku-connectors-telegram`
  - Extended inbound update parsing to support `callback_query` in addition to plain `message` updates.
  - Added structured approval callback decoding so Telegram button clicks map into `ApprovalDecision` operations.
  - Added inline approval keyboards on `pending_approval` responses, allowing approve/reject directly from Telegram.
  - Added `answerCallbackQuery` handling so callback interactions receive immediate acknowledgement at the Telegram API layer.
- `roku-cmd`
  - Replaced the single closure-based Telegram handler with an explicit runtime-backed interaction handler that can execute requests and resolve approval decisions.

### Verification Status

- `cargo fmt --all`: passed
- `cargo test -p roku-connectors-telegram -p roku-cmd -p roku-runtime-service -p roku-e2e`: passed
- `cargo check --workspace`: passed

### Remaining Work

- Telegram rich rendering is still text-first; artifact and experiment results are not yet rendered as richer diagnostic cards.
- Long-running task progress push is still missing from the Telegram transport.
- LLM provider cost / latency observability and planning-time llm-adapter integration remain open.

### Next Recommended Steps

1. Add TG artifact / experiment rich rendering and long-task status push.
2. Add LLM provider telemetry into `roku-observability` and surface it from the OpenRouter path.
3. Move planning outline generation onto the llm-adapter path so the runtime matches the planning architecture in the design doc.

## 2026-03-07 - Session Milestone (Phase 26)

### Completed Modules

- `roku-observability`
  - Added LLM invocation metrics to the shared `Metrics` model, including request totals, success/failure counts, routing-failure counts, token totals, cumulative latency, cumulative estimated cost, and per-provider/model breakdown snapshots.
- `roku-llm-adapter`
  - Added observability hooks in `LlmRouter` so successful and failed provider calls emit structured metrics.
  - Added explicit routing-failure accounting for cases where no model is eligible before a provider call happens.
  - Added an OpenRouter builder variant that accepts a shared metrics handle, so live execution can report into the runtime's observability plane.
- `roku-runtime-service`
  - Switched the runtime service to hold `Arc<Metrics>` so the live runtime path and orchestration path can share the same metrics object.
- `roku-cmd`
  - Live runtime bootstrap now wires a shared metrics handle through OpenRouter router construction and runtime-service creation.

### Verification Status

- `cargo fmt --all`: passed
- `cargo test -p roku-observability -p roku-llm-adapter -p roku-connectors-telegram -p roku-cmd -p roku-runtime-service`: passed
- `cargo check --workspace`: passed
- `cargo clippy --workspace --all-targets -- -D warnings`: passed

### Remaining Work

- Provider metrics are now in place, but tool-runtime cost / timeout / deny-rate metrics are still not implemented.
- Planning still uses heuristic inputs rather than the live llm-adapter path.
- Telegram transport still lacks long-task progress push and richer artifact / experiment rendering.

### Next Recommended Steps

1. Move planning outline generation and reasoning selection onto `roku-llm-adapter`.
2. Extend `roku-tool-runtime` and `roku-observability` with tool execution cost / timeout / deny-rate metrics.
3. Add richer Telegram artifact / experiment rendering and task-progress push.

## 2026-03-07 - Session Milestone (Phase 27)

### Completed Modules

- `roku-task-planner`
  - Added `LlmTaskPlanner`, which requests a structured `PlanOutline` through `roku-llm-adapter`, normalizes the returned JSON, and falls back to the deterministic planner when the model output is invalid.
  - Added parser coverage for both valid structured JSON and fallback behavior.
- `roku-runtime-service`
  - Replaced the fixed `PlanningInput` constants with request-derived planning input estimation, so planning mode selection now reacts to goal complexity, uncertainty, and risk cues.
  - Generalized runtime-service planner injection from a concrete planner type to a trait-object boundary, which keeps planning-layer swaps outside the orchestration core.
- `roku-cmd`
  - Live runtime bootstrap now wires both a live execution router and a live LLM-backed planner into the same runtime service, sharing one metrics handle.

### Verification Status

- `cargo fmt --all`: passed
- `cargo test -p roku-task-planner -p roku-runtime-service -p roku-cmd -p roku-e2e`: passed
- `cargo check --workspace`: passed
- `cargo clippy --workspace --all-targets -- -D warnings`: passed

### Remaining Work

- Planning outline generation is now on the llm-adapter path for the live bootstrap, but coding-provider routing still does not use the same router abstraction.
- Planning strategy selection is now request-derived, but tree-search / refinement branch scoring is still heuristic rather than feedback-driven.
- Telegram rich rendering and long-task progress push remain open.

### Next Recommended Steps

1. Extend `roku-llm-adapter` into coding-provider selection so `LLM-08` is fully closed.
2. Add tool-runtime timeout / deny-rate / cost telemetry to complete the remaining observability gap.
3. Add richer Telegram artifact / experiment rendering and progress callbacks.

## 2026-03-07 - Session Milestone (Phase 28)

### Completed Modules

- `roku-llm-adapter`
  - Reworked OpenRouter response parsing away from the previous rigid untagged enum so the adapter now accepts string content, array content, object-shaped content, and `reasoning` fallback when providers return `content: null`.
  - Added runtime-side stderr diagnostics for provider HTTP failures, parse failures, latency, and token counts.
- `roku-runtime-service`
  - Added minimal stderr request/planning logs so live execution now emits request id, planning mode, derived planning input, and outline step count.
- `roku-connectors-telegram`
  - Added stderr diagnostics around inbound update handling and outbound response delivery so Telegram polling runs are debuggable from the server side.
- `roku-cmd`
  - Added explicit bot startup log for the Telegram polling command.

### Verification Status

- `cargo fmt --all`: passed
- `cargo test -p roku-llm-adapter -p roku-connectors-telegram -p roku-runtime-service -p roku-cmd`: passed
- `cargo check --workspace`: passed
- `cargo clippy --workspace --all-targets -- -D warnings`: passed
- Live smoke:
  - `cargo run -p roku-cmd -- live-once '哎哎哎！你现在是 kiki 还是 roku 呀？'`
  - Result: `我是 roku。`

### Remaining Work

- Logging is now minimally useful, but it is still stderr-first rather than a full structured sink/exporter.
- Telegram long-task progress push and richer artifact / experiment rendering remain open.
- Coding-provider routing still does not use the unified llm-adapter selection path.

### Next Recommended Steps

1. Replace the current stderr-first diagnostics with a structured logging / exporter path that still works well locally.
2. Add Telegram rich artifact / experiment rendering and long-task progress callbacks.
3. Extend `roku-llm-adapter` into coding-provider selection to close `LLM-08`.

## 2026-03-07 - Session Milestone (Phase 29)

### Completed Modules

- `roku-connectors-telegram`
  - Made the polling loop resilient to transient Telegram API failures so a single `getUpdates` or `sendMessage` error no longer terminates the bot process.
  - Shortened approval callback payloads from the previous verbose format to a compact `ap:<action>:<id>` format.
  - Split Telegram HTTP client errors into client-build vs request-time failures so runtime diagnostics are no longer misleading.
- `roku-runtime-service`
  - Replaced verbose approval ids with compact deterministic ids to keep Telegram callback payloads within the platform's 64-byte button-data limit.
  - Added a regression test that explicitly guards the callback-data length constraint.

### Verification Status

- `cargo fmt --all`: passed
- `cargo test -p roku-connectors-telegram -p roku-runtime-service -p roku-cmd -p roku-e2e`: passed
- `cargo check --workspace`: passed
- `cargo clippy --workspace --all-targets -- -D warnings`: passed

### Remaining Work

- Telegram transport is now more robust, but long-task progress push and richer artifact / experiment rendering still remain.
- Logging is still stderr-first and should eventually move onto a structured sink/exporter path.
- Coding-provider routing still does not use the unified llm-adapter path.

### Next Recommended Steps

1. Add Telegram rich artifact / experiment rendering and long-task progress callbacks.
2. Move stderr-first diagnostics onto structured logging/exporters while keeping local usability.
3. Extend `roku-llm-adapter` into coding-provider selection to close `LLM-08`.

## 2026-03-07 - Session Milestone (Phase 30)

### Completed Modules

- `roku-task-planner`
  - Added a deterministic fast-path for `ReAct` mode so short conversational Telegram requests no longer spend an extra live LLM call generating a plan outline.
- `roku-agent-runtime`
  - Raised the live LLM tool timeout budget from the previous 20s default to 45s, which better matches the observed latency variance of free OpenRouter models.

### Verification Status

- `cargo fmt --all`: passed
- `cargo test -p roku-task-planner -p roku-agent-runtime -p roku-runtime-service -p roku-cmd`: passed
- `cargo check --workspace`: passed
- `cargo clippy --workspace --all-targets -- -D warnings`: passed
- Live smoke:
  - `cargo run -p roku-cmd -- live-once '所以你现在到底是 roku 还是 kiki'`
  - Result: `I am Roku.`
  - Only execution-path LLM calls remained; the extra live planning call for `ReAct` mode was removed.

### Remaining Work

- Telegram long-task progress push and richer artifact / experiment rendering remain open.
- Logging is still stderr-first and should eventually move onto a structured sink/exporter path.
- Coding-provider routing still does not use the unified llm-adapter path.

### Next Recommended Steps

1. Add Telegram rich artifact / experiment rendering and long-task progress callbacks.
2. Move stderr-first diagnostics onto structured logging/exporters while keeping local usability.
3. Extend `roku-llm-adapter` into coding-provider selection to close `LLM-08`.

## 2026-03-08 - Session Milestone (Phase 31)

### Completed Modules

- `roku-llm-adapter`
  - Switched the default OpenRouter route from the generic `openrouter/free` router to an explicit model chain headed by `step-3.5-flash:free`, with fallbacks for `deepseek-chat` and `gemini-2.0-flash`.
  - Normalized common shorthand model ids into OpenRouter-compatible ids so local env configuration can stay concise while runtime requests stay explicit.
  - Moved request construction onto an explicit OpenAI-compatible chat-completions shape with separate `system` and `user` messages instead of flattening everything into one user prompt.
  - Added support for OpenRouter `models[]` request fallback chains and surfaced both `requested_model` and `served_model` in runtime logs.
  - Updated the optional OpenRouter app-name header to the documented `X-OpenRouter-Title` variant.
- `roku-task-planner`
  - Started sending planning requests with an explicit system instruction so model output remains constrained to the expected JSON outline contract.
- `roku-agent-runtime`
  - Tightened live worker prompting so final user-visible responses no longer leak internal execution-role framing, hidden instructions, or runtime metadata.
  - Anchored the final conversational persona to `Roku` for direct user-facing replies.
- `roku-connectors-telegram`
  - Added a configurable poll-error suppression threshold so transient `getUpdates` failures stay quiet during routine operation.
  - Preserved explicit poll warnings for sustained outages by logging only at configured consecutive-failure boundaries.
- Local runtime configuration
  - Updated `.env` defaults for the local machine to use the new OpenRouter primary/fallback chain and the Telegram poll-noise threshold.

### Verification Status

- `cargo fmt --all`: passed
- `cargo test -p roku-llm-adapter -p roku-agent-runtime -p roku-task-planner -p roku-runtime-service -p roku-cmd -p roku-connectors-telegram`: passed
- `cargo check --workspace`: passed
- `cargo clippy --workspace --all-targets -- -D warnings`: passed
- Live smoke:
  - `cargo run -p roku-cmd -- live-once '所以你现在到底是 roku 还是 kiki'`
  - Result: `我是 Roku。`
  - Observed log shape: `requested_model=stepfun/step-3.5-flash:free served_model=stepfun/step-3.5-flash:free`

### Remaining Work

- Coding-provider routing still does not use the unified llm-adapter selection path.
- Structured logging/export sinks are still not in place; current diagnostics remain stderr-first.

### Next Recommended Steps

1. Close `LLM-08` by routing coding-provider model selection through `roku-llm-adapter`.
2. Revisit structured logging/export sinks now that the Telegram response surface is richer and includes progress receipts.
3. Expand Telegram UX further only if a stronger progress protocol or artifact actions are needed.

## 2026-03-08 - Session Milestone (Phase 34)

### Completed Modules

- `roku-connectors-telegram`
  - Added an explicit `running` progress receipt that is sent immediately after a user request is accepted.
  - Preserved the final response as a separate completion receipt, so Telegram users now see a start-of-work acknowledgement and an end-of-work result.
  - Added regression coverage for the new progress notice formatting.

### Verification Status

- `cargo fmt --all`: passed
- `cargo test -p roku-connectors-telegram -p roku-cmd -p roku-runtime-service -p roku-e2e`: passed
- `cargo check --workspace`: passed
- `cargo clippy --workspace --all-targets -- -D warnings`: passed

### Remaining Work

- Coding-provider routing still does not use the unified llm-adapter selection path.
- Structured logging/export sinks are still not in place; current diagnostics remain stderr-first.

### Next Recommended Steps

1. Close `LLM-08` by routing coding-provider model selection through `roku-llm-adapter`.
2. Revisit structured logging/export sinks now that the Telegram response surface is richer and includes progress receipts.
3. Expand Telegram UX further only if a stronger progress protocol or artifact actions are needed.

## 2026-03-08 - Session Milestone (Phase 33)

### Completed Modules

- `roku-connectors-telegram`
  - Upgraded response rendering from flat plain text into MarkdownV2-rich sections for status, request id, message body, approvals, artifacts, experiments, and generic references.
  - Kept approval buttons intact while rendering the approval id and action hint as structured rich text.
  - Added URI-aware attachment grouping so `artifact://`, `experiment://`, and generic references are shown in separate sections.
  - Added Markdown escaping so request ids, artifact URIs, experiment references, and arbitrary messages do not break Telegram formatting.

### Verification Status

- `cargo fmt --all`: passed
- `cargo test -p roku-connectors-telegram -p roku-cmd -p roku-runtime-service -p roku-e2e`: passed
- `cargo check --workspace`: passed
- `cargo clippy --workspace --all-targets -- -D warnings`: passed

### Remaining Work

- Coding-provider routing still does not use the unified llm-adapter selection path.
- Telegram still lacks long-task progress push.
- Structured logging/export sinks are still not in place; current diagnostics remain stderr-first.

### Next Recommended Steps

1. Close `LLM-08` by routing coding-provider model selection through `roku-llm-adapter`.
2. Implement `TG-08` so Telegram can stream long-task progress instead of only final status text.
3. Revisit structured logging/export sinks once the Telegram progress path is in place.

## 2026-03-08 - Session Milestone (Phase 32)

### Completed Modules

- `roku-llm-adapter`
  - Added explicit `ProviderCallError` typing so provider failures are no longer opaque strings.
  - Added router-level `ProviderResiliencePolicy` with bounded retry, exponential backoff, and circuit-breaker state tracking.
  - Centralized provider resilience in `LlmRouter`, so future providers inherit the same retry/backoff/circuit-open behavior without duplicating logic.
  - Added regression coverage for retryable recovery, non-retryable short-circuiting, breaker opening, and cooldown probing.
- `roku-agent-runtime`
  - Surfaced `LlmAdapterError::CircuitOpen` as a clear tool failure so runtime errors remain understandable upstream.

### Verification Status

- `cargo fmt --all`: passed
- `cargo test -p roku-llm-adapter -p roku-task-planner -p roku-agent-runtime -p roku-runtime-service -p roku-cmd`: passed
- `cargo check --workspace`: passed
- `cargo clippy --workspace --all-targets -- -D warnings`: passed
- Live smoke:
  - `cargo run -p roku-cmd -- live-once '所以你现在到底是 roku 还是 kiki'`
  - Result: `我是Roku。`

### Remaining Work

- Coding-provider routing still does not use the unified llm-adapter selection path.
- Telegram still lacks rich artifact / experiment rendering and long-task progress push.
- Structured logging/export sinks are still not in place; current diagnostics remain stderr-first.

### Next Recommended Steps

1. Close `LLM-08` by routing coding-provider model selection through `roku-llm-adapter`.
2. Implement `TG-07` and `TG-08` so Telegram can render artifacts and stream long-task progress instead of only final status text.
3. Revisit structured logging/export sinks once the Telegram artifact/progress path is in place.
