[![animation](docs/static/git-cliff-anim.gif)](https://git-cliff.org)

# Changelog

## [v0.0.2] - 2026-03-11

### Features

- **(roku-agent-runtime)** route skill installs through workers (by @[itscheems](https://github.com/itscheems)) - ([79edcbf](https://github.com/itscheems/Roku/commit/79edcbf387418eb35c6897c3f01f7f9aeff578de))
- **(roku-api-gateway)** expose replay diagnostics (by @[itscheems](https://github.com/itscheems)) - ([1992cf4](https://github.com/itscheems/Roku/commit/1992cf49155cdda7412d72c3c1ab1053312cd4b4))
- **(roku-cmd)** add telegram preview command (by @[itscheems](https://github.com/itscheems)) - ([387a12b](https://github.com/itscheems/Roku/commit/387a12b5409a143276716fe9bf73f2662ac078d1))
- **(roku-cmd)** add direct skill registry commands (by @[itscheems](https://github.com/itscheems)) - ([da10f76](https://github.com/itscheems/Roku/commit/da10f76c47f04081caf7afac34534135a6abec6c))
- **(roku-cmd)** configure project-local skill storage (by @[itscheems](https://github.com/itscheems)) - ([425502b](https://github.com/itscheems/Roku/commit/425502b877799e2c0f2c7ccf2ed9cc8ff663c199))
- **(roku-cmd)** add artifact and replay ops commands (by @[itscheems](https://github.com/itscheems)) - ([d4cd4c5](https://github.com/itscheems/Roku/commit/d4cd4c584c93f38f45fc1d7836640f4cdf80d4f8))
- **(roku-execution-graph-builder)** compile conditional graph edges (by @[itscheems](https://github.com/itscheems)) - ([c54464c](https://github.com/itscheems/Roku/commit/c54464c2f52c5caecf2cb247810ed012e1bf310c))
- **(roku-execution-graph-builder)** inject retry and dead-letter helpers (by @[itscheems](https://github.com/itscheems)) - ([7f10e06](https://github.com/itscheems/Roku/commit/7f10e06c45d60dde3942d8ea56c3dabc8fe6bc2a))
- **(roku-execution-graph-builder)** add aggregation gate (by @[itscheems](https://github.com/itscheems)) - ([0df55dc](https://github.com/itscheems/Roku/commit/0df55dc95bb24fb09407fbc2ec8c50a5bda2cb2f))
- **(roku-recovery)** add replay metadata baseline (by @[itscheems](https://github.com/itscheems)) - ([006a5d5](https://github.com/itscheems/Roku/commit/006a5d5832dea3312fb2a597d6a9c47a57b85b6e))
- **(roku-runtime)** expand direct filesystem routing (by @[itscheems](https://github.com/itscheems)) - ([f5e9ce6](https://github.com/itscheems/Roku/commit/f5e9ce63f0ac73638088e96b98329ff09c2b7b9d))
- **(roku-runtime)** add core direct-route tools (by @[itscheems](https://github.com/itscheems)) - ([8abb497](https://github.com/itscheems/Roku/commit/8abb497054db80d819c8cf3e9ade03216954b0e6))
- **(roku-runtime-service)** rebuild recovery from event stream (by @[itscheems](https://github.com/itscheems)) - ([0534aec](https://github.com/itscheems/Roku/commit/0534aecc67ef49b2ad73b8d1c6b67f247ebc0721))
- **(roku-runtime-service)** reconstruct helper progress from persistence (by @[itscheems](https://github.com/itscheems)) - ([9236581](https://github.com/itscheems/Roku/commit/9236581a5e37b96aa8461b3b56ea71209ff12889))
- **(roku-runtime-service)** enforce node budget snapshots (by @[itscheems](https://github.com/itscheems)) - ([eed62a6](https://github.com/itscheems/Roku/commit/eed62a676a3dede6f53ae9f912dc228c9da4f2f0))
- **(roku-runtime-service)** reconstruct recovery progress from results (by @[itscheems](https://github.com/itscheems)) - ([2e45ed7](https://github.com/itscheems/Roku/commit/2e45ed7cd171f649f35224ea4a3b204c7b066ae8))
- **(roku-runtime-service)** add cancel and timeout recovery flow (by @[itscheems](https://github.com/itscheems)) - ([60bbfa3](https://github.com/itscheems/Roku/commit/60bbfa3abe4daa8e737b35ef888b3a74d8a60066))
- **(roku-runtime-service)** route scheduling through dispatch queue (by @[itscheems](https://github.com/itscheems)) - ([a6f5d4f](https://github.com/itscheems/Roku/commit/a6f5d4fd4b433ffc7e0f6efb0e8e98debcc23075))
- **(roku-runtime-service)** add resumable task recovery (by @[itscheems](https://github.com/itscheems)) - ([4ad2b62](https://github.com/itscheems/Roku/commit/4ad2b62087a41365a68a8f5c70003af87d2c8938))
- **(roku-skill-registry)** add file-backed skill installation (by @[itscheems](https://github.com/itscheems)) - ([f696e42](https://github.com/itscheems/Roku/commit/f696e42dfb6eae79cc206987f982ed93456ca745))
- **(roku-state-store)** compact replay logs into snapshots (by @[itscheems](https://github.com/itscheems)) - ([f518780](https://github.com/itscheems/Roku/commit/f518780c59df18d380e5b570da7ac271e2720e59))
- **(roku-supervisor-agent)** own final completion policy (by @[itscheems](https://github.com/itscheems)) - ([8762753](https://github.com/itscheems/Roku/commit/8762753e9893eb3070fa79226446ce89eaed21f9))
- **(roku-supervisor-agent)** extract planning boundary (by @[itscheems](https://github.com/itscheems)) - ([2b9720b](https://github.com/itscheems/Roku/commit/2b9720b7198a3e583f22cfec3d51d82d81658082))
- **(skill-runtime)** add layered skill shortcut routing (by @[itscheems](https://github.com/itscheems)) - ([4a09cdf](https://github.com/itscheems/Roku/commit/4a09cdf1945635583a42847c6bb704a88f5cdb47))

### Bug Fixes

- **(roku-agent-runtime)** ground installed skill answers (by @[itscheems](https://github.com/itscheems)) - ([cd1bf65](https://github.com/itscheems/Roku/commit/cd1bf652500e6b2b75ba6ecc7b89ec788bb08511))
- **(roku-runtime)** stabilize resource routing and service restart (by @[itscheems](https://github.com/itscheems)) - ([0c720e7](https://github.com/itscheems/Roku/commit/0c720e7cbbb8683b0d1f17fc23a8b7681bda4a6c))
- **(roku-skill)** restore single-root skill discovery and execution (by @[itscheems](https://github.com/itscheems)) - ([14f46a1](https://github.com/itscheems/Roku/commit/14f46a1b1982cd50e44a916a837b4bedf0730167))
- **(roku-skill-registry)** keep large installed skills activatable (by @[itscheems](https://github.com/itscheems)) - ([413ae19](https://github.com/itscheems/Roku/commit/413ae19247f14089b92092006e769887e6b75024))
- **(roku-state-store)** support rusqlite 0.38 sqlite integer conversions (by @[itscheems](https://github.com/itscheems)) - ([6597ff8](https://github.com/itscheems/Roku/commit/6597ff8dba0aa96fa102dfe9ae3bf0e873805a55))
- **(roku-telegram)** keep bot sessions on direct routes (by @[itscheems](https://github.com/itscheems)) - ([2bc0e47](https://github.com/itscheems/Roku/commit/2bc0e4758b4c6e0647cdf07960fab032cc061baf))
- **(skill-runtime)** stabilize installed skill activation (by @[itscheems](https://github.com/itscheems)) - ([653f90c](https://github.com/itscheems/Roku/commit/653f90c734ac1e95109d7fb5ff3b40ca8639f3e8))

### Refactor

- **(local-storage)** adopt sqlite-first runtime storage (by @[itscheems](https://github.com/itscheems)) - ([7242cd5](https://github.com/itscheems/Roku/commit/7242cd569b6435f8c775a9cc84d81803ab98cc90))
- **(roku-agent-runtime)** add direct route classifier (by @[itscheems](https://github.com/itscheems)) - ([9889103](https://github.com/itscheems/Roku/commit/9889103964ea8444d995d237be2fe8525fc43907))
- **(roku-agent-runtime)** extract tool config and rewire plugin crates (by @[itscheems](https://github.com/itscheems)) - ([6024c75](https://github.com/itscheems/Roku/commit/6024c75292e0a60deb7782b86766b82907e5766c))
- **(roku-plugin-catalog)** migrate resource catalog and skill registry (by @[itscheems](https://github.com/itscheems)) - ([bf5c4a4](https://github.com/itscheems/Roku/commit/bf5c4a4fdf15cce615e965b99f26cb986f472f97))
- **(roku-plugin-host)** migrate tool runtime into plugin host (by @[itscheems](https://github.com/itscheems)) - ([69b9914](https://github.com/itscheems/Roku/commit/69b9914c64a0c0470926d5b0ee430610b5768336))
- **(roku-plugin-llm)** add structured route parse guards (by @[itscheems](https://github.com/itscheems)) - ([836803f](https://github.com/itscheems/Roku/commit/836803f5d2943b7c9a4fc8cb99e8baa44f9b8b18))
- **(roku-plugin-providers)** migrate mcp coding and llm adapters (by @[itscheems](https://github.com/itscheems)) - ([ee2995d](https://github.com/itscheems/Roku/commit/ee2995de8f084cce334b8542c860130d2f421c27))
- **(roku-plugin-telegram)** migrate telegram connector (by @[itscheems](https://github.com/itscheems)) - ([e60386f](https://github.com/itscheems/Roku/commit/e60386f45b91f366b63f4c9f481473cc53c81984))
- **(roku-plugins)** remove llm adapter shim crate (by @[itscheems](https://github.com/itscheems)) - ([cd05c2a](https://github.com/itscheems/Roku/commit/cd05c2ae27a13d361dd7365381b6b55c41268610))
- **(roku-plugins)** remove tool runtime shim crate (by @[itscheems](https://github.com/itscheems)) - ([999f1e2](https://github.com/itscheems/Roku/commit/999f1e237bbd026fdb34c45c3734e14cc141f7b9))
- **(roku-plugins)** remove resource catalog shim crate (by @[itscheems](https://github.com/itscheems)) - ([e83e14d](https://github.com/itscheems/Roku/commit/e83e14d4daeefb00d4f39aaaa9a50f23f6ac5f2a))
- **(roku-plugins)** remove skill registry shim crate (by @[itscheems](https://github.com/itscheems)) - ([1dd67eb](https://github.com/itscheems/Roku/commit/1dd67ebe0f50dca740b2e2631572954155e8ce32))
- **(roku-plugins)** remove mcp shim crate (by @[itscheems](https://github.com/itscheems)) - ([5705f0d](https://github.com/itscheems/Roku/commit/5705f0d547d2833469679dec0684eac08a4a4784))
- **(roku-plugins)** remove coding adapter shim crate (by @[itscheems](https://github.com/itscheems)) - ([e2d0869](https://github.com/itscheems/Roku/commit/e2d0869bad07e21895d6eb0cfa1f4d3d53f97f5c))
- **(roku-plugins)** remove telegram shim crate (by @[itscheems](https://github.com/itscheems)) - ([432bac6](https://github.com/itscheems/Roku/commit/432bac6499e773f2379e078380235478ee7f706a))
- **(roku-plugins)** add startup-governed plugin snapshot (by @[itscheems](https://github.com/itscheems)) - ([e501c74](https://github.com/itscheems/Roku/commit/e501c7458e23eb22ce50979fef3a1475950f647c))
- **(roku-plugins)** align plugin layout with refactor plan (by @[itscheems](https://github.com/itscheems)) - ([ce3ad08](https://github.com/itscheems/Roku/commit/ce3ad086f4938e75c6b1de9a22860f25265049b4))
- **(roku-runtime)** rewire command and service consumers (by @[itscheems](https://github.com/itscheems)) - ([4eb0afd](https://github.com/itscheems/Roku/commit/4eb0afd11537fb111d65766d277b566fd865ac73))
- **(roku-runtime)** route through resource catalogs (by @[itscheems](https://github.com/itscheems)) - ([6b4606a](https://github.com/itscheems/Roku/commit/6b4606a99c206380985839b274b532e02630ac80))
- **(roku-runtime-service)** retire legacy planning crates (by @[itscheems](https://github.com/itscheems)) - ([caa3499](https://github.com/itscheems/Roku/commit/caa3499f8c2f38155e7386e90ad074bb204d8e45))
- **(roku-runtime-service)** default to direct route execution (by @[itscheems](https://github.com/itscheems)) - ([03c9d3b](https://github.com/itscheems/Roku/commit/03c9d3b6b67136b72fe3e22f5fb81d6f2689b2fc))
- **(roku-runtime-service)** centralize replay reporting (by @[itscheems](https://github.com/itscheems)) - ([b6ca53d](https://github.com/itscheems/Roku/commit/b6ca53dc0246556f5b523bd9f14989d58f993ac5))

### Documentation

- **(phase-1)** record closure decision (by @[itscheems](https://github.com/itscheems)) - ([6c82103](https://github.com/itscheems/Roku/commit/6c82103ec2ec4f825b89e02e3b59236999e6f98b))
- **(skill-runtime)** capture operator commands and validation findings (by @[itscheems](https://github.com/itscheems)) - ([a8db407](https://github.com/itscheems/Roku/commit/a8db4075a299aab86df6c0dddfff755124519d6e))
- **(skill-runtime)** record installation slice progress (by @[itscheems](https://github.com/itscheems)) - ([48d51a4](https://github.com/itscheems/Roku/commit/48d51a4ab94cc3c15b1040d9117beb17c0fb1216))

### Testing

- **(roku-e2e)** close control-plane chaos coverage (by @[itscheems](https://github.com/itscheems)) - ([69a138b](https://github.com/itscheems/Roku/commit/69a138bfb71de46e7d01226a520cc60bbc7d9e80))

### Miscellaneous Tasks

- **(roku-e2e)** remove obsolete e2e crate (by @[itscheems](https://github.com/itscheems)) - ([640f271](https://github.com/itscheems/Roku/commit/640f27198992c7c1457b9363e4afac840f327f8b))
- **(roku-llm)** default openrouter to deepseek-chat (by @[itscheems](https://github.com/itscheems)) - ([50cf400](https://github.com/itscheems/Roku/commit/50cf4000edfe89a28555d49110d13929bfbee025))
- **(roku-plugins)** scaffold plugin namespace crates (by @[itscheems](https://github.com/itscheems)) - ([3e46ebe](https://github.com/itscheems/Roku/commit/3e46ebe8625d641851c2eaf72e109748532a1ae7))

## New Contributors ❤️

* @github-actions[bot] made their first contribution
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
