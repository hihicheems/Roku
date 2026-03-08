[![animation](docs/static/git-cliff-anim.gif)](https://git-cliff.org)

# Changelog

## [v0.0.1] - 2026-03-08

### Features

- **(roku-agent-runtime)** route built-in workers through tool runtime (by @[itscheems](https://github.com/itscheems)) - ([568ba52](https://github.com/itscheems/Roku/commit/568ba52055df2e146c1a0b37852a80dc12d72fde))
- **(roku-agent-runtime)** add capability-dispatched worker registry (by @[itscheems](https://github.com/itscheems)) - ([deb65d1](https://github.com/itscheems/Roku/commit/deb65d1a9438e782b12a82b27c753df9150643c0))
- **(roku-api-gateway)** add artifact content and download endpoints (by @[itscheems](https://github.com/itscheems)) - ([8c5075c](https://github.com/itscheems/Roku/commit/8c5075cfd53d43ae7715ef7014ab2b650adf3d09))
- **(roku-api-gateway)** expose task artifacts and experiment state (by @[itscheems](https://github.com/itscheems)) - ([98fb766](https://github.com/itscheems/Roku/commit/98fb76660919176c61bc7660158c2898a4ddd237))
- **(roku-api-gateway)** add approval decision endpoints (by @[itscheems](https://github.com/itscheems)) - ([596d48e](https://github.com/itscheems/Roku/commit/596d48e276e11dd9c70457aa018d410d76f14c53))
- **(roku-api-gateway)** bind runtime service executor (by @[itscheems](https://github.com/itscheems)) - ([2495cb5](https://github.com/itscheems/Roku/commit/2495cb54f8451c12c60139b317ba94e74a48234c))
- **(roku-api-gateway)** add actix HTTP routes and executor boundary (by @[itscheems](https://github.com/itscheems)) - ([2eb7312](https://github.com/itscheems/Roku/commit/2eb7312c1355bf40060786b499bd3495bc619928))
- **(roku-cmd)** add task and approval ops commands (by @[itscheems](https://github.com/itscheems)) - ([97882f8](https://github.com/itscheems/Roku/commit/97882f86096c056c04920e4a9c4dad5d2c01c3a7))
- **(roku-cmd)** add api gateway service mode (by @[itscheems](https://github.com/itscheems)) - ([d117295](https://github.com/itscheems/Roku/commit/d117295e16a10750ffe5a2cf9a4a1c4c4575a810))
- **(roku-cmd)** add planning mode override flags (by @[itscheems](https://github.com/itscheems)) - ([e319437](https://github.com/itscheems/Roku/commit/e3194376617c182aa2ae47633564ab269e9ea5e6))
- **(roku-cmd)** wire session-aware planning and file logging (by @[itscheems](https://github.com/itscheems)) - ([7bc9835](https://github.com/itscheems/Roku/commit/7bc9835781b7568810fc5b3b8cf93263835cc623))
- **(roku-common-types)** add artifact and experiment contracts (by @[itscheems](https://github.com/itscheems)) - ([948a437](https://github.com/itscheems/Roku/commit/948a4373376324f33ec9d6fa5e31c1e96803785d))
- **(roku-common-types)** track completed nodes for DAG resume (by @[itscheems](https://github.com/itscheems)) - ([11e4607](https://github.com/itscheems/Roku/commit/11e46079124052064a97a2f5fd5f4083e4b32d3c))
- **(roku-common-types)** add approval and task checkpoint contracts (by @[itscheems](https://github.com/itscheems)) - ([c934f91](https://github.com/itscheems/Roku/commit/c934f91a1eca3f3994c1a0c65e5575f09bea284c))
- **(roku-common-types)** add serde-ready runtime contracts (by @[itscheems](https://github.com/itscheems)) - ([529e92d](https://github.com/itscheems/Roku/commit/529e92dc30c27df0bfd46ef7f2b7280725cc8c88))
- **(roku-connectors-telegram)** support inline planning commands (by @[itscheems](https://github.com/itscheems)) - ([da54a52](https://github.com/itscheems/Roku/commit/da54a52d41bcc5fb67a5c2ca4306a2753978a569))
- **(roku-connectors-telegram)** simplify telegram user-facing responses (by @[itscheems](https://github.com/itscheems)) - ([a3a7fb1](https://github.com/itscheems/Roku/commit/a3a7fb14aac4c1e6c6d2d54e5b3aac9cbeca45fe))
- **(roku-connectors-telegram)** add progress receipts (by @[itscheems](https://github.com/itscheems)) - ([09a5235](https://github.com/itscheems/Roku/commit/09a5235485d209b3f5180fd2802f4259f73e645b))
- **(roku-connectors-telegram)** add rich attachment rendering (by @[itscheems](https://github.com/itscheems)) - ([6c9d52d](https://github.com/itscheems/Roku/commit/6c9d52d53820a59e452cf879c55acfa1bf30fe15))
- **(roku-connectors-telegram)** support approval callback actions (by @[itscheems](https://github.com/itscheems)) - ([0d12632](https://github.com/itscheems/Roku/commit/0d126326f2a1d4de1bd8c6b07ecd8e2003cb9b7c))
- **(roku-connectors-telegram)** add live telegram polling bootstrap (by @[itscheems](https://github.com/itscheems)) - ([9bc968a](https://github.com/itscheems/Roku/commit/9bc968ad2c332263e86ab30b265cb218364c9d13))
- **(roku-connectors-telegram)** add webhook and outbound adapters (by @[itscheems](https://github.com/itscheems)) - ([3acb03e](https://github.com/itscheems/Roku/commit/3acb03ea1c25f8ca364b1bcffeca7d40f0fd4835))
- **(roku-execution-graph-builder)** compile dependency-aware task graphs (by @[itscheems](https://github.com/itscheems)) - ([f44e123](https://github.com/itscheems/Roku/commit/f44e1239e505a026b4a2e8cf24e47ceec01268bc))
- **(roku-execution-graph-builder)** add DAG scheduler modules (by @[itscheems](https://github.com/itscheems)) - ([77562d5](https://github.com/itscheems/Roku/commit/77562d585a357157334637fe526edc2ac4e0a8e6))
- **(roku-llm-adapter)** add provider resilience controls (by @[itscheems](https://github.com/itscheems)) - ([4083543](https://github.com/itscheems/Roku/commit/4083543b20e500e2f792890850ddb36ddc8e0eab))
- **(roku-llm-adapter)** add openrouter model chain and chat roles (by @[itscheems](https://github.com/itscheems)) - ([5fa50c8](https://github.com/itscheems/Roku/commit/5fa50c8fe72ab5987bf7023388863207a416b92c))
- **(roku-llm-adapter)** add openrouter live runtime path (by @[itscheems](https://github.com/itscheems)) - ([e58ab7e](https://github.com/itscheems/Roku/commit/e58ab7ec3beea00b02b5d478e726900e6ec2d850))
- **(roku-llm-adapter)** add risk and budget-aware provider routing (by @[itscheems](https://github.com/itscheems)) - ([c3ffe67](https://github.com/itscheems/Roku/commit/c3ffe67ce708edafb8a32f060c666bfa361abbeb))
- **(roku-observability)** timestamp component log files (by @[itscheems](https://github.com/itscheems)) - ([bc090f7](https://github.com/itscheems/Roku/commit/bc090f7ef2b93d9088acd8a80d245b2d8912d5a3))
- **(roku-observability)** add llm provider metrics (by @[itscheems](https://github.com/itscheems)) - ([aac107f](https://github.com/itscheems/Roku/commit/aac107f498d6846cbaca042de93bc1bc6695c99f))
- **(roku-observability)** add planning metrics and audit correlation (by @[itscheems](https://github.com/itscheems)) - ([9051142](https://github.com/itscheems/Roku/commit/90511421130716a9facdf7bdbcba9e75db4c570c))
- **(roku-observability)** track artifact and experiment metrics (by @[itscheems](https://github.com/itscheems)) - ([2525534](https://github.com/itscheems/Roku/commit/2525534415dcd7f7d757b74b1258ab19285c1500))
- **(roku-observability)** track approval and dead-letter metrics (by @[itscheems](https://github.com/itscheems)) - ([021ddb9](https://github.com/itscheems/Roku/commit/021ddb98abc7c30a975a1d70ff94bf448b1bec95))
- **(roku-observability)** add audit sinks and metrics snapshots (by @[itscheems](https://github.com/itscheems)) - ([f0b0fd5](https://github.com/itscheems/Roku/commit/f0b0fd514f8118ea04929701a079138f9ff7df07))
- **(roku-planning-engine)** add loop controls and planning hooks (by @[itscheems](https://github.com/itscheems)) - ([e969bbf](https://github.com/itscheems/Roku/commit/e969bbfe7c58b5a81f93c5a3ee8d118b46308853))
- **(roku-runtime)** scaffold roku modules and implement core execution flow (by @[itscheems](https://github.com/itscheems)) - ([8307897](https://github.com/itscheems/Roku/commit/8307897a64d50a56b0bf99048930770cfef6bba2))
- **(roku-runtime-service)** add explicit node aggregation contracts (by @[itscheems](https://github.com/itscheems)) - ([beef89d](https://github.com/itscheems/Roku/commit/beef89d9f5e8edfaea695650a3ce46afe8a43e24))
- **(roku-runtime-service)** persist artifacts and experiment runs (by @[itscheems](https://github.com/itscheems)) - ([0a495e0](https://github.com/itscheems/Roku/commit/0a495e00debcea9eea58a157a658f71c8df3bbcf))
- **(roku-runtime-service)** validate from node-scoped results (by @[itscheems](https://github.com/itscheems)) - ([474f369](https://github.com/itscheems/Roku/commit/474f369ce9247ebf75f622691f6580647e2dfcfa))
- **(roku-runtime-service)** schedule tasks from completed node state (by @[itscheems](https://github.com/itscheems)) - ([747a0aa](https://github.com/itscheems/Roku/commit/747a0aa4f7413b7039f218b231ebc04a7d0dbd5f))
- **(roku-runtime-service)** persist approvals and resume tasks (by @[itscheems](https://github.com/itscheems)) - ([5f569a2](https://github.com/itscheems/Roku/commit/5f569a21cc503690bf63cdd3d019f302dbd8827b))
- **(roku-runtime-service)** add approval and dead-letter flows (by @[itscheems](https://github.com/itscheems)) - ([4d963a7](https://github.com/itscheems/Roku/commit/4d963a702f8bb50893f0f4ef2e1c981e9e91ad16))
- **(roku-runtime-service)** extract reusable execution service (by @[itscheems](https://github.com/itscheems)) - ([5ec4145](https://github.com/itscheems/Roku/commit/5ec414591805f4c1e404d43a744d614a6b18c864))
- **(roku-state-store)** add postgres orchestration backends (by @[itscheems](https://github.com/itscheems)) - ([e0a135b](https://github.com/itscheems/Roku/commit/e0a135bde84b3a1ae60d6adbdff867a3c9216e58))
- **(roku-state-store)** add postgres session repositories (by @[itscheems](https://github.com/itscheems)) - ([8b70b45](https://github.com/itscheems/Roku/commit/8b70b450a9f274a94dec45d61278e9908673928b))
- **(roku-state-store)** add node-scoped result repositories (by @[itscheems](https://github.com/itscheems)) - ([62f8212](https://github.com/itscheems/Roku/commit/62f82129c4445a7c5b2bced982eecdf3cea92208))
- **(roku-state-store)** add approval ticket repositories (by @[itscheems](https://github.com/itscheems)) - ([83086ca](https://github.com/itscheems/Roku/commit/83086caebd12b21cdc51d3c4a3cb58cdab3ec739))
- **(roku-state-store)** add trait-backed repositories and file adapters (by @[itscheems](https://github.com/itscheems)) - ([9470348](https://github.com/itscheems/Roku/commit/94703486046bae2b926abdd79723a5b2844de2b7))
- **(roku-task-planner)** add llm-backed live planning (by @[itscheems](https://github.com/itscheems)) - ([7baa0df](https://github.com/itscheems/Roku/commit/7baa0df219f4014cdf135c670ef7ab11c5a4a65c))
- **(roku-tool-runtime)** add descriptor-based execution policies (by @[itscheems](https://github.com/itscheems)) - ([de09fc7](https://github.com/itscheems/Roku/commit/de09fc7769821cd941d54caed21327fbea22af8a))
- **(roku-validation-plane)** validate artifact-backed evidence (by @[itscheems](https://github.com/itscheems)) - ([9f06d70](https://github.com/itscheems/Roku/commit/9f06d70b49e116d8703b23ff8fd061c031a57dea))
- **(roku-validation-plane)** add staged validation pipeline (by @[itscheems](https://github.com/itscheems)) - ([22ee28a](https://github.com/itscheems/Roku/commit/22ee28ac490e25bc192a63a75051c7bad71ad8b4))
- **(roku-workspace)** add artifact and experiment data-plane crates (by @[itscheems](https://github.com/itscheems)) - ([4db7ad1](https://github.com/itscheems/Roku/commit/4db7ad130a3c7c88d4edda26c1ca3672c870afa8))

### Bug Fixes

- **(roku-agent-runtime)** harden live reply surfacing (by @[itscheems](https://github.com/itscheems)) - ([94952c0](https://github.com/itscheems/Roku/commit/94952c00f4c4ab8928bfc9e73a7d34c64704db8c))
- **(roku-agent-runtime)** ground live replies with runtime context (by @[itscheems](https://github.com/itscheems)) - ([5c0d0ea](https://github.com/itscheems/Roku/commit/5c0d0eaea070407cbae83d7cfeac37e327497712))
- **(roku-connectors-telegram)** suppress routine polling noise (by @[itscheems](https://github.com/itscheems)) - ([04437ff](https://github.com/itscheems/Roku/commit/04437ff043a2dfefdd43fbc3e9554f93c12d24cf))
- **(roku-connectors-telegram)** harden polling and callback payloads (by @[itscheems](https://github.com/itscheems)) - ([91d1fff](https://github.com/itscheems/Roku/commit/91d1fff94c86a9d644e3cc2cb59d087cf8849cb0))
- **(roku-llm-adapter)** recover unreadable openrouter responses (by @[itscheems](https://github.com/itscheems)) - ([be66377](https://github.com/itscheems/Roku/commit/be663779957102cc81d4bd941bb82df99184ce30))
- **(roku-llm-adapter)** recover openrouter reasoning payloads (by @[itscheems](https://github.com/itscheems)) - ([2986b87](https://github.com/itscheems/Roku/commit/2986b87e85b9e74c6fb443de981f21004102e4c6))
- **(roku-task-planner)** stabilize short live telegram chats (by @[itscheems](https://github.com/itscheems)) - ([c975ab7](https://github.com/itscheems/Roku/commit/c975ab700b826b3fc0bf6441b02f713e39a6aec2))
- **(roku-workspace)** grant release workflow pull request access (by @[itscheems](https://github.com/itscheems)) - ([515a2de](https://github.com/itscheems/Roku/commit/515a2de234724c552eca3851b140c5e5a11da587))

### Refactor

- **(roku-api-gateway)** split gateway into HTTP modules (by @[itscheems](https://github.com/itscheems)) - ([bced5e2](https://github.com/itscheems/Roku/commit/bced5e2ac016c1358747297a02544922fb1b6c52))
- **(roku-runtime-service)** split execution and data-plane modules (by @[itscheems](https://github.com/itscheems)) - ([5b5f917](https://github.com/itscheems/Roku/commit/5b5f917aa8f237136216a4d589e1b9697be4ef22))
- **(roku-runtime-service)** split helpers and tests into modules (by @[itscheems](https://github.com/itscheems)) - ([d1fb29a](https://github.com/itscheems/Roku/commit/d1fb29a5987b66ec6e6d0b381730409fdaaacf90))
- **(roku-tool-runtime)** split runtime modules (by @[itscheems](https://github.com/itscheems)) - ([7afa973](https://github.com/itscheems/Roku/commit/7afa973076a0cd1d96f9ae53bc99da1b3d8d6b00))
- **(roku-validation-plane)** modularize validation stages (by @[itscheems](https://github.com/itscheems)) - ([c2eed5c](https://github.com/itscheems/Roku/commit/c2eed5cd3d7fb55e7bfd7796838895e60c8b5f23))
- **(roku-workspace)** modularize planner and provider boundaries (by @[itscheems](https://github.com/itscheems)) - ([d682b91](https://github.com/itscheems/Roku/commit/d682b912dad12ca438bff6357015d1f59cb6d29b))

### Documentation

- **(roku-dev-log)** record service topology milestone (by @[itscheems](https://github.com/itscheems)) - ([9fa4180](https://github.com/itscheems/Roku/commit/9fa41804c319e6523cd9001c5e864538762617c2))
- **(roku-dev-log)** record telegram ux and dev service milestone (by @[itscheems](https://github.com/itscheems)) - ([c0f2a9a](https://github.com/itscheems/Roku/commit/c0f2a9a449f009eea011c46a38f5ee46db7b4321))
- **(roku-dev-log)** record phase-12 planning checkpoint (by @[itscheems](https://github.com/itscheems)) - ([2ea5bb3](https://github.com/itscheems/Roku/commit/2ea5bb353e9281c7bb1c5cff1eabe81932e15031))
- **(roku-dev-log)** record phase-11 telegram connector checkpoint (by @[itscheems](https://github.com/itscheems)) - ([7148c59](https://github.com/itscheems/Roku/commit/7148c59e8eade4c46c043fb4a2aca8a74e3dd141))
- **(roku-dev-log)** record phase-10 graph builder checkpoint (by @[itscheems](https://github.com/itscheems)) - ([6d591bc](https://github.com/itscheems/Roku/commit/6d591bc6d98c470ea77f4eae47a0743969266674))
- **(roku-dev-log)** record phase-9 aggregation checkpoint (by @[itscheems](https://github.com/itscheems)) - ([1a1aaa3](https://github.com/itscheems/Roku/commit/1a1aaa3ec4e73a457bb53fed776d29523265ca04))
- **(roku-dev-log)** record phase-7 and phase-8 checkpoints (by @[itscheems](https://github.com/itscheems)) - ([774d6fd](https://github.com/itscheems/Roku/commit/774d6fd5b88345ce0ff8491dd0ac9225a0a24a5e))
- **(roku-dev-log)** record phase-6 result-store checkpoint (by @[itscheems](https://github.com/itscheems)) - ([7d045e8](https://github.com/itscheems/Roku/commit/7d045e81a3fe4b4f0fe1fcc994374be7a8d1aa84))
- **(roku-dev-log)** record phase-5 scheduler checkpoint (by @[itscheems](https://github.com/itscheems)) - ([cb4a523](https://github.com/itscheems/Roku/commit/cb4a52307ad943414ad34c371616585072995a3e))
- **(roku-dev-log)** record phase-4 approval checkpoint (by @[itscheems](https://github.com/itscheems)) - ([7081586](https://github.com/itscheems/Roku/commit/70815860b830b3f23549afc13f5fa33f5278b505))
- **(roku-dev-log)** record phase-3 runtime checkpoint (by @[itscheems](https://github.com/itscheems)) - ([07b6f2d](https://github.com/itscheems/Roku/commit/07b6f2df40859bf4eb38dade2fde0b5133ee8543))
- **(roku-dev-log)** add phase-2 checkpoint for adapters and failure paths (by @[itscheems](https://github.com/itscheems)) - ([94daa95](https://github.com/itscheems/Roku/commit/94daa95d4cfdb3652ee782947247a9bea56a94cf))
- **(roku-dev-log)** record implemented modules and verification status (by @[itscheems](https://github.com/itscheems)) - ([28de767](https://github.com/itscheems/Roku/commit/28de767bff68fda9a262a1b27aa4a234a338d3de))
- **(roku-todo-list)** align roadmap with design architecture (by @[itscheems](https://github.com/itscheems)) - ([0cea540](https://github.com/itscheems/Roku/commit/0cea540c1c82aced2227b373c29ccd3cfa910a1b))
- **(roku-todo-list)** add project roadmap (by @[itscheems](https://github.com/itscheems)) - ([e4fd80b](https://github.com/itscheems/Roku/commit/e4fd80b6c418e50c9f5723c032eb3c029f996758))

### Testing

- **(roku-e2e)** cover task data HTTP endpoints (by @[itscheems](https://github.com/itscheems)) - ([d672940](https://github.com/itscheems/Roku/commit/d6729408e609261527bdf4cdfb9d249be47acab3))
- **(roku-e2e)** add end-to-end happy path integration test (by @[itscheems](https://github.com/itscheems)) - ([8619fe6](https://github.com/itscheems/Roku/commit/8619fe624956cee147d6fd3937ab06a2362f938e))
- **(roku-runtime)** add failure-mode runtime and e2e coverage (by @[itscheems](https://github.com/itscheems)) - ([90d8191](https://github.com/itscheems/Roku/commit/90d819114dbf90bb422fe193dca95bea507bee42))

### Miscellaneous Tasks

- **(roku-workspace)** normalize source headers with just fmt (by @[itscheems](https://github.com/itscheems)) - ([89cfed5](https://github.com/itscheems/Roku/commit/89cfed5448564d125ae6c21d22a816512c868451))
- **(roku-workspace)** refine dev service topology (by @[itscheems](https://github.com/itscheems)) - ([a2d9f2e](https://github.com/itscheems/Roku/commit/a2d9f2e252b5888207e2a16bdbe0ce8555ebd6aa))
- **(roku-workspace)** expand doctor service topology (by @[itscheems](https://github.com/itscheems)) - ([cad45d7](https://github.com/itscheems/Roku/commit/cad45d757e615eb0e870db42806533e203883d0e))
- **(roku-workspace)** add dev service orchestration commands (by @[itscheems](https://github.com/itscheems)) - ([f33fca7](https://github.com/itscheems/Roku/commit/f33fca7c731029006d4026d9f5b505e8e1065fd9))
- **(roku-workspace)** format graph builder after validation (by @[itscheems](https://github.com/itscheems)) - ([81e166a](https://github.com/itscheems/Roku/commit/81e166ae322a60ae7ad1dcc4d599b0fd04da0281))
- **(roku-workspace)** format gateway and e2e files (by @[itscheems](https://github.com/itscheems)) - ([2d49456](https://github.com/itscheems/Roku/commit/2d49456a262162310720e977d4e8c212bae089f2))
- **(roku-workspace)** format data-plane crates (by @[itscheems](https://github.com/itscheems)) - ([e784d9a](https://github.com/itscheems/Roku/commit/e784d9af851f1c47474725a4fb09b4ab37fa43e1))
- update CHANGELOG.md (by @[itscheems](https://github.com/itscheems)) - ([65706c0](https://github.com/itscheems/Roku/commit/65706c0555603a6b35618a20375074c83bb93b4f))
- update .gitignore (by @[itscheems](https://github.com/itscheems)) - ([fa672f9](https://github.com/itscheems/Roku/commit/fa672f9edf09e3b81073d4b2c464c83e508ac06d))

## New Contributors ❤️

* @itscheems made their first contribution
* @dependabot[bot] made their first contribution<!-- generated by git-cliff -->
