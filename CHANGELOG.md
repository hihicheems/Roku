[![animation](docs/static/git-cliff-anim.gif)](https://git-cliff.org)

# Changelog

## [v0.0.6] - 2026-03-18

### Features

- **(roku-cmd)** use sqlite adapter for telegram memory state (by @[itscheems](https://github.com/itscheems)) - ([46fee37](https://github.com/itscheems/Roku/commit/46fee37fcd9422408a624dd19d54bb952b377455))
- **(roku-cmd)** wire OpenViking memory backend commands (by @[itscheems](https://github.com/itscheems)) - ([a60fdb8](https://github.com/itscheems/Roku/commit/a60fdb8ba03b4e199163a2aa9cdb4bc4b9ca29b1))
- **(roku-cmd)** prepare managed openviking config at startup (by @[itscheems](https://github.com/itscheems)) - ([e491a55](https://github.com/itscheems/Roku/commit/e491a559ed2fd0ab2656ad36b3fc5f823adf0d3b))
- **(roku-control-plane)** add provider-neutral control-plane core (by @[itscheems](https://github.com/itscheems)) - ([6ba149d](https://github.com/itscheems/Roku/commit/6ba149d45a878e36fda22fdafeaf434d2adba1d1))
- **(roku-entry-registry)** assemble entry runtime bundles (by @[itscheems](https://github.com/itscheems)) - ([511e85c](https://github.com/itscheems/Roku/commit/511e85c49fb48a76d44610ce38e78ecd047af0ff))
- **(roku-memory)** add provider-neutral entry registry API (by @[itscheems](https://github.com/itscheems)) - ([7f780cf](https://github.com/itscheems/Roku/commit/7f780cf6981db3b3e10164b96a6d64129af46735))
- **(roku-memory)** own provider-neutral memory config and test backends (by @[itscheems](https://github.com/itscheems)) - ([fea6b61](https://github.com/itscheems/Roku/commit/fea6b6128dd32e5857cbf57223d021ce828c4c9c))
- **(roku-memory)** add entry registry resolution (by @[itscheems](https://github.com/itscheems)) - ([733094f](https://github.com/itscheems/Roku/commit/733094f240f832fe7f5ed501ae45525564c68358))
- **(roku-memory)** add adapter availability registry helpers (by @[itscheems](https://github.com/itscheems)) - ([8adea2d](https://github.com/itscheems/Roku/commit/8adea2d72f91f19542b19de91f0988198e1c6b98))
- **(roku-memory)** add continuity and registry contract modules (by @[itscheems](https://github.com/itscheems)) - ([6eb324a](https://github.com/itscheems/Roku/commit/6eb324a3be022f441f83d6103ad6d56776fd23f4))
- **(roku-memory)** define long-term memory contracts (by @[itscheems](https://github.com/itscheems)) - ([14c04e1](https://github.com/itscheems/Roku/commit/14c04e156c75864acb66231169d1ff0997014f8d))
- **(roku-plugin-control-plane-sqlite)** add sqlite control-plane adapter (by @[itscheems](https://github.com/itscheems)) - ([1f50b3f](https://github.com/itscheems/Roku/commit/1f50b3f468545085e604da54511b465e500e24a6))
- **(roku-plugin-memory-openviking)** expose registry subsystem registration (by @[itscheems](https://github.com/itscheems)) - ([4f0b54d](https://github.com/itscheems/Roku/commit/4f0b54d31a8dd9381ec569c3131a58f572dbde28))
- **(roku-plugin-memory-openviking)** add runtime-state adapter capability (by @[itscheems](https://github.com/itscheems)) - ([5d75cfe](https://github.com/itscheems/Roku/commit/5d75cfe7405ce01e12623be6bc384895a4f2595e))
- **(roku-plugin-memory-openviking)** expose adapter registration surface (by @[itscheems](https://github.com/itscheems)) - ([387485e](https://github.com/itscheems/Roku/commit/387485e61653e682b4bbf4eea6530ceb538f209a))
- **(roku-plugin-memory-openviking)** own backend-scoped runtime config (by @[itscheems](https://github.com/itscheems)) - ([5e39462](https://github.com/itscheems/Roku/commit/5e39462dd16e4729112e4476cb2c4a54dbc05d26))
- **(roku-plugin-memory-openviking)** add OpenViking memory backend adapter (by @[itscheems](https://github.com/itscheems)) - ([fe13224](https://github.com/itscheems/Roku/commit/fe1322482649e2b8a90fdc11d6bc2991f98165bb))
- **(roku-plugin-memory-sqlite)** expose registry subsystem registration (by @[itscheems](https://github.com/itscheems)) - ([6610cdc](https://github.com/itscheems/Roku/commit/6610cdccc55f40d7d5177c1b6178eeb7694118e4))
- **(roku-plugin-memory-sqlite)** expose registry-facing subsystem resolution (by @[itscheems](https://github.com/itscheems)) - ([7931f37](https://github.com/itscheems/Roku/commit/7931f37413d4bc14951b07e4938037344c502390))
- **(roku-plugin-memory-sqlite)** add sqlite memory adapter crate (by @[itscheems](https://github.com/itscheems)) - ([1497c7e](https://github.com/itscheems/Roku/commit/1497c7e733a3a8b3a55c8530877c447539f68e1c))
- **(roku-runtime-service)** assemble context bundles for memory hooks (by @[itscheems](https://github.com/itscheems)) - ([9e585f7](https://github.com/itscheems/Roku/commit/9e585f7fc91774d9b0e3778ef4eb515ea7517296))
- **(roku-state-store)** implement concrete repos for memory contracts (by @[itscheems](https://github.com/itscheems)) - ([f3edc77](https://github.com/itscheems/Roku/commit/f3edc77d44166b3395a272de05c6e31ed046cdd4))

### Bug Fixes

- **(roku-cmd)** document memory startup residue boundaries (by @[itscheems](https://github.com/itscheems)) - ([aee3e87](https://github.com/itscheems/Roku/commit/aee3e874086b00609c21f0dcee6a17dcd3300545))
- **(roku-cmd)** clarify transitional memory bootstrap (by @[itscheems](https://github.com/itscheems)) - ([ab37f9c](https://github.com/itscheems/Roku/commit/ab37f9c9ca0a955982015d8add02db89141e7e82))
- **(roku-cmd)** consume adapter registration metadata (by @[itscheems](https://github.com/itscheems)) - ([74646d9](https://github.com/itscheems/Roku/commit/74646d9ddf249f59ef979a8424fed4f7b1d94555))
- **(roku-cmd)** require explicit memory scope identity (by @[itscheems](https://github.com/itscheems)) - ([05a8ad5](https://github.com/itscheems/Roku/commit/05a8ad51619fcacc11969d64cf836a72c5ab5165))
- **(roku-cmd)** resolve clippy issues in memory runtime config (by @[itscheems](https://github.com/itscheems)) - ([0d7330b](https://github.com/itscheems/Roku/commit/0d7330b254499113bb75719287617e2fa45cbb36))
- **(roku-cmd)** harden openviking config generation (by @[itscheems](https://github.com/itscheems)) - ([929c2d9](https://github.com/itscheems/Roku/commit/929c2d91a3b7e3738286f23e4cf4b9ae700348eb))
- **(roku-common-types)** clarify legacy request memory semantics (by @[itscheems](https://github.com/itscheems)) - ([137e7d0](https://github.com/itscheems/Roku/commit/137e7d0662e1d7a6cb26ca70a8a18d0faf994705))
- **(roku-common-types)** clarify legacy memory carrier semantics (by @[itscheems](https://github.com/itscheems)) - ([5d5930d](https://github.com/itscheems/Roku/commit/5d5930d3cd178f4946872bdd14550a638dfddb78))
- **(roku-memory)** align registry resolution terminology (by @[itscheems](https://github.com/itscheems)) - ([f92588c](https://github.com/itscheems/Roku/commit/f92588c83b8938deb1a5d79188fbfce467ab0bcc))
- **(roku-memory)** align phase1-3 contract semantics (by @[itscheems](https://github.com/itscheems)) - ([b17f0e8](https://github.com/itscheems/Roku/commit/b17f0e8febf618d20324e88ad0460637aa19cb75))
- **(roku-plugin-memory-openviking)** clarify managed process wording (by @[itscheems](https://github.com/itscheems)) - ([147cee1](https://github.com/itscheems/Roku/commit/147cee1a0459186c7f0c3e0769c004eef5136bb4))
- **(roku-plugin-memory-openviking)** normalize sidecar search hits (by @[itscheems](https://github.com/itscheems)) - ([e8e4da3](https://github.com/itscheems/Roku/commit/e8e4da39c7465350ce463eee82eb44b660964cb6))
- **(roku-plugin-memory-sqlite)** clarify adapter resolution terminology (by @[itscheems](https://github.com/itscheems)) - ([abe3817](https://github.com/itscheems/Roku/commit/abe381788cd2635954634a209b8efa58f05c904d))
- **(roku-plugin-tools)** clarify short-term conversation carrier semantics (by @[itscheems](https://github.com/itscheems)) - ([04f2a81](https://github.com/itscheems/Roku/commit/04f2a8191774788ff9d59c031f34096aa217b5cf))
- **(roku-runtime-service)** preserve long-term recall boundaries (by @[itscheems](https://github.com/itscheems)) - ([22a800f](https://github.com/itscheems/Roku/commit/22a800f20c8c43b4a64fd98ae7b656465a32175f))
- **(roku-state-store)** document exit from target memory architecture (by @[itscheems](https://github.com/itscheems)) - ([f4d7d43](https://github.com/itscheems/Roku/commit/f4d7d43bb1d8ac9f879bea9743cd52dd6c3c0159))

### Refactor

- **(roku-cmd)** harden memory startup boundaries (by @[itscheems](https://github.com/itscheems)) - ([83160ab](https://github.com/itscheems/Roku/commit/83160ab83442f15b6e88c8bcc40110944746e64e))
- **(roku-cmd)** route startup through memory entry registry (by @[itscheems](https://github.com/itscheems)) - ([6c907f1](https://github.com/itscheems/Roku/commit/6c907f1b5050413bee11c21176dd223f79a4334c))
- **(roku-cmd)** resolve runtime startup through entry registry (by @[itscheems](https://github.com/itscheems)) - ([6cd0608](https://github.com/itscheems/Roku/commit/6cd0608bfef4d57f1b1ec905884182402bcc37bb))
- **(roku-cmd)** consume core-owned memory config (by @[itscheems](https://github.com/itscheems)) - ([af62901](https://github.com/itscheems/Roku/commit/af62901395513402eecf2d2e6a3d169b2bee6975))
- **(roku-cmd)** route entry memory wiring through registry (by @[itscheems](https://github.com/itscheems)) - ([89fae0b](https://github.com/itscheems/Roku/commit/89fae0bb8413bd326f9f572875bbc60261370545))
- **(roku-cmd)** wire default long-term memory bootstrap (by @[itscheems](https://github.com/itscheems)) - ([8d7583c](https://github.com/itscheems/Roku/commit/8d7583c974fd7b272c931750607a82b0f845c797))
- **(roku-cmd)** align telegram continuity terminology (by @[itscheems](https://github.com/itscheems)) - ([afe2510](https://github.com/itscheems/Roku/commit/afe251021a21be4eea66f03a9dccc0b66103a8b9))
- **(roku-cmd)** separate telegram continuity from session state (by @[itscheems](https://github.com/itscheems)) - ([6da9d2a](https://github.com/itscheems/Roku/commit/6da9d2a9c50c3e8ccc4df5132abd389b56ce27c0))
- **(roku-memory)** move control-plane contracts into memory (by @[itscheems](https://github.com/itscheems)) - ([3d19587](https://github.com/itscheems/Roku/commit/3d19587c73571bb317fc986afe4edab54fcb0910))
- **(roku-memory)** reorganize memory contracts into subdomains (by @[itscheems](https://github.com/itscheems)) - ([eeaa8ee](https://github.com/itscheems/Roku/commit/eeaa8ee3bdcecaab4bdac918afa72be69f6ba119))
- **(roku-plugin-memory-openviking)** separate adapter registration from backend (by @[itscheems](https://github.com/itscheems)) - ([41a33a2](https://github.com/itscheems/Roku/commit/41a33a213ad2cacb95cf812a4051365d60eeff4b))
- **(roku-plugin-memory-sqlite)** absorb sqlite control-plane adapter (by @[itscheems](https://github.com/itscheems)) - ([bbbf18c](https://github.com/itscheems/Roku/commit/bbbf18c886fe76a7099a6b9a659642229a123c97))
- **(roku-plugin-memory-sqlite)** own sqlite memory persistence (by @[itscheems](https://github.com/itscheems)) - ([3426c3c](https://github.com/itscheems/Roku/commit/3426c3c2baf729cc7bdc97c51611d035370b797b))
- **(roku-plugin-memory-sqlite)** map sqlite repos through adapter contracts (by @[itscheems](https://github.com/itscheems)) - ([a187fdf](https://github.com/itscheems/Roku/commit/a187fdfe066ec3e80fddb3626d5898c649710198))
- **(roku-runtime-service)** consume control-plane bundles (by @[itscheems](https://github.com/itscheems)) - ([67a2cc7](https://github.com/itscheems/Roku/commit/67a2cc7f53c3574abac7ee1cbf24c253629f1535))
- **(roku-runtime-service)** route long-term context outside conversation history (by @[itscheems](https://github.com/itscheems)) - ([8dd2b4d](https://github.com/itscheems/Roku/commit/8dd2b4d3cb7358362c781ef605c96948be329cff))
- **(roku-runtime-service)** accept trait-object memory wiring (by @[itscheems](https://github.com/itscheems)) - ([501d3ed](https://github.com/itscheems/Roku/commit/501d3ed40d64852ec86f506d27efb6a5669a32a6))
- **(roku-state-store)** drop memory contract implementations from repos (by @[itscheems](https://github.com/itscheems)) - ([8282b1d](https://github.com/itscheems/Roku/commit/8282b1d4d56b846df9c7447234fb538ae4c81179))
- **(roku-state-store)** add session-state and continuity store boundaries (by @[itscheems](https://github.com/itscheems)) - ([8a3f5db](https://github.com/itscheems/Roku/commit/8a3f5dbe04a05b867b23310f4a3c239161d23697))
- **(scripts)** align sqlite reporting with canonical memory precedence (by @[itscheems](https://github.com/itscheems)) - ([0e32f38](https://github.com/itscheems/Roku/commit/0e32f38c032c7ee0b583fac833900559663e10cd))
- **(workspace)** remove transitional control-plane crates (by @[itscheems](https://github.com/itscheems)) - ([9891bd7](https://github.com/itscheems/Roku/commit/9891bd72b5de964096e37271084625daf0e6a8f9))
- **(workspace)** remove roku-state-store from the workspace (by @[itscheems](https://github.com/itscheems)) - ([98b7be4](https://github.com/itscheems/Roku/commit/98b7be416d03768d2d943cbbbe49b6a5f2812f94))

### Documentation

- **(roku-memory)** document core memory contracts (by @[itscheems](https://github.com/itscheems)) - ([37edd10](https://github.com/itscheems/Roku/commit/37edd105421457ca8e0dbd6f4baaef0f44bb5adb))
- **(roku-plugin-memory-openviking)** document adapter boundaries (by @[itscheems](https://github.com/itscheems)) - ([ded8adb](https://github.com/itscheems/Roku/commit/ded8adb9c24689fc5b1c2600c7b1316550b99d9d))

### Testing

- **(roku-cmd)** prove telegram transport uses memory subsystem seams (by @[itscheems](https://github.com/itscheems)) - ([f11352c](https://github.com/itscheems/Roku/commit/f11352c9631f46b0100c60e646c4b6efb482137d))
- **(roku-cmd)** cover memory registry resolution and config layering (by @[itscheems](https://github.com/itscheems)) - ([dadcd4b](https://github.com/itscheems/Roku/commit/dadcd4b50671c2557f2226a6cbd004239619896a))
- **(roku-memory)** add entry registry boundary guardrails (by @[itscheems](https://github.com/itscheems)) - ([29f75c9](https://github.com/itscheems/Roku/commit/29f75c92017d5cc328ecddc4c23d6f076bfae148))
- **(roku-state-store)** cover semantic session-state boundaries (by @[itscheems](https://github.com/itscheems)) - ([08d8bd0](https://github.com/itscheems/Roku/commit/08d8bd00a81b7cb9e5ff73c45dc48158e20456f7))

### Miscellaneous Tasks

- **(deploy)** pass proxy env into docker validate build (by @[itscheems](https://github.com/itscheems)) - ([a917047](https://github.com/itscheems/Roku/commit/a917047984cc6f65d13f7abccadd980c5e0a20a6))
- **(deploy)** work around docker validate network timeouts (by @[itscheems](https://github.com/itscheems)) - ([d8b7819](https://github.com/itscheems/Roku/commit/d8b78193ff31927d05a0fabce76bbd3ee82a6bea))
- **(deploy)** optimize docker validate workflow (by @[itscheems](https://github.com/itscheems)) - ([138db2a](https://github.com/itscheems/Roku/commit/138db2a59b6a3b8d3b524299c0f6c754592c0ee4))
- **(roku-cmd)** add local OpenViking dev helper script (by @[itscheems](https://github.com/itscheems)) - ([36f32c1](https://github.com/itscheems/Roku/commit/36f32c1b3b99b0104cdf82e3ffccf7bb700db50c))
- update .gitignore (by @[itscheems](https://github.com/itscheems)) - ([4c76a1a](https://github.com/itscheems/Roku/commit/4c76a1a18c00e9fe936865a8d5b428c9e52a4223))

## [v0.0.5] - 2026-03-15

### Features

- **(deploy)** add docker build scripts and shell checks (by @[itscheems](https://github.com/itscheems)) - ([4581354](https://github.com/itscheems/Roku/commit/45813541a01292e79a07eae6939ee282cf27d2c3))
- **(deploy)** support multi-arch docker builds (by @[itscheems](https://github.com/itscheems)) - ([8480367](https://github.com/itscheems/Roku/commit/84803671358003f52276c02d530aa1b141a0f12b))
- **(roku-plugin-telegram)** add session control commands (by @[itscheems](https://github.com/itscheems)) - ([391ad29](https://github.com/itscheems/Roku/commit/391ad2927210149855615dd1bc7eff42acd2aa86))
- **(roku-plugin-tools)** unify python and web tool contracts (by @[itscheems](https://github.com/itscheems)) - ([ddfe3b6](https://github.com/itscheems/Roku/commit/ddfe3b6d7c2bfaaf4e8a413ec05f9989bde6fd75))
- **(roku-runtime)** add constrained command execution probe (by @[itscheems](https://github.com/itscheems)) - ([afb1645](https://github.com/itscheems/Roku/commit/afb1645ae8d87bfeb63b8cea0199df9f8e4f3301))

### Bug Fixes

- **(roku-cmd)** use unique request ids for probe runs (by @[itscheems](https://github.com/itscheems)) - ([d9bfc48](https://github.com/itscheems/Roku/commit/d9bfc48440c651b92600d4a592c3b3569a244d2e))
- **(roku-runtime)** harden loop fallbacks and budgets (by @[itscheems](https://github.com/itscheems)) - ([9278a4a](https://github.com/itscheems/Roku/commit/9278a4a1cd87448089f56b98453ef8e2bac873c9))
- **(roku-runtime)** tighten explicit resource grounding (by @[itscheems](https://github.com/itscheems)) - ([6a46ddb](https://github.com/itscheems/Roku/commit/6a46ddb0dd01f7d72a0d1e93f3e35758a2c95aa1))
- **(roku-runtime)** ask for missing direct tool inputs (by @[itscheems](https://github.com/itscheems)) - ([7f61a7e](https://github.com/itscheems/Roku/commit/7f61a7e62aa56e97ade4da8a61b456910a2d1686))
- **(roku-runtime)** tighten entry-stage tool binding (by @[itscheems](https://github.com/itscheems)) - ([4270300](https://github.com/itscheems/Roku/commit/4270300b1d9b5351032674ca0f11145064446156))
- **(roku-runtime)** align tool routing with contract semantics (by @[itscheems](https://github.com/itscheems)) - ([e5c9e20](https://github.com/itscheems/Roku/commit/e5c9e205c1f40e81eb34003f56cef8145d193d19))

### Refactor

- **(roku-common-types)** freeze shared tool contracts (by @[itscheems](https://github.com/itscheems)) - ([841754f](https://github.com/itscheems/Roku/commit/841754feafe384f9d5435f54d23f7597850e3a0a))
- **(roku-plugin-tools)** migrate builtins to unified contracts (by @[itscheems](https://github.com/itscheems)) - ([5b07cee](https://github.com/itscheems/Roku/commit/5b07ceebcb85fa301207a3ccb6c87d8559ddb440))
- **(roku-plugins)** separate canonical descriptors from selection hints (by @[itscheems](https://github.com/itscheems)) - ([d80aaf8](https://github.com/itscheems/Roku/commit/d80aaf86fe9604f4d60e518c2155531a5764e9e0))
- **(roku-runtime)** tighten paused loop resume contracts (by @[itscheems](https://github.com/itscheems)) - ([55da7ca](https://github.com/itscheems/Roku/commit/55da7caeea260069ad30ab9035c2224cf1173c48))
- **(roku-runtime)** use structured general completion contracts (by @[itscheems](https://github.com/itscheems)) - ([ea3389d](https://github.com/itscheems/Roku/commit/ea3389d508f41b054721ae2e8e64ae1aa5264594))
- **(roku-runtime)** demote grounding to explicit resource alignment (by @[itscheems](https://github.com/itscheems)) - ([c887043](https://github.com/itscheems/Roku/commit/c887043fea66d0ffe667686921aafb484e098a56))
- **(roku-runtime)** compact selection hint consumption (by @[itscheems](https://github.com/itscheems)) - ([3577b28](https://github.com/itscheems/Roku/commit/3577b283fcde17bfd7cde7bd4145df051d76a33a))
- **(roku-runtime)** formalize loop trace contracts (by @[itscheems](https://github.com/itscheems)) - ([b42ae57](https://github.com/itscheems/Roku/commit/b42ae57c052705a5bb734af6e4f2dcc1075274a8))

### Documentation

- **(deploy)** add bilingual deployment guides (by @[itscheems](https://github.com/itscheems)) - ([6c0d389](https://github.com/itscheems/Roku/commit/6c0d389bee7c0a43f4eda0bc3a8f7c208f88fba1))
- **(roku-cmd)** document bootstrap and storage boundaries (by @[itscheems](https://github.com/itscheems)) - ([711b11d](https://github.com/itscheems/Roku/commit/711b11d85acb8be080abfccd37f6187ec76f14be))
- **(roku-plugin-telegram)** document control command boundaries (by @[itscheems](https://github.com/itscheems)) - ([0a9ceb7](https://github.com/itscheems/Roku/commit/0a9ceb76efcc8215d362f5e2d970d5afb9f09935))

### Testing

- **(roku-runtime)** cover controlled family seeds and ambiguity recovery (by @[itscheems](https://github.com/itscheems)) - ([a1fd4e8](https://github.com/itscheems/Roku/commit/a1fd4e8d24c4440ebcaba98f2dfa01e0f7494f47))
- **(roku-runtime)** cover missing-input direct tool routes (by @[itscheems](https://github.com/itscheems)) - ([3a6d999](https://github.com/itscheems/Roku/commit/3a6d999acab3288dfdcf8888837a4b52d90dd1dd))
- **(roku-runtime)** cover python and web tool admission (by @[itscheems](https://github.com/itscheems)) - ([260a42b](https://github.com/itscheems/Roku/commit/260a42b61dc09f6e778657b106e84687936423ef))
- **(roku-runtime-service)** cover stale freeform loop recovery (by @[itscheems](https://github.com/itscheems)) - ([6e251b5](https://github.com/itscheems/Roku/commit/6e251b50991df8cee2a2343f078b321cc2c50b71))
- **(roku-validation-plane)** grade runtime loop trace payloads (by @[itscheems](https://github.com/itscheems)) - ([52794e3](https://github.com/itscheems/Roku/commit/52794e3afca9b1ee858a30448a678d0da5cec211))

### Miscellaneous Tasks

- **(repo)** ignore codex workspace state (by @[itscheems](https://github.com/itscheems)) - ([bfc123f](https://github.com/itscheems/Roku/commit/bfc123f4dfc325bc5aa0a489521375592f9652a7))

## [v0.0.4] - 2026-03-14

### Features

- **(roku-plugins)** add typed runtime config for plugin crates (by @[itscheems](https://github.com/itscheems)) - ([ead97b7](https://github.com/itscheems/Roku/commit/ead97b7c89ac78cf2cb8fcd3b4b82a6812497554))

### Bug Fixes

- **(roku-plugin-tools)** move terminal tool semantics into catalog metadata (by @[itscheems](https://github.com/itscheems)) - ([1d92d04](https://github.com/itscheems/Roku/commit/1d92d0485be1833dd0f32de2edad051256871b27))
- **(roku-plugin-tools)** improve fuzzy filesystem grounding (by @[itscheems](https://github.com/itscheems)) - ([7b19c86](https://github.com/itscheems/Roku/commit/7b19c8674aca0d202061ac1bfb4217a4c7f4214d))
- **(roku-runtime)** derive loop termination from tool contracts (by @[itscheems](https://github.com/itscheems)) - ([e7a7607](https://github.com/itscheems/Roku/commit/e7a7607ecce7a24193f86a01184c69f63f97c7a8))
- **(roku-runtime-service)** resume pending loops through their original driver (by @[itscheems](https://github.com/itscheems)) - ([3e12ac5](https://github.com/itscheems/Roku/commit/3e12ac55acc712606b9094cae1fb59d55039a512))

### Refactor

- **(roku-agent-runtime)** add typed config for loop and routing budgets (by @[itscheems](https://github.com/itscheems)) - ([02cd472](https://github.com/itscheems/Roku/commit/02cd4725999d34edd425db3e9213bfcc7aa3ca26))
- **(roku-agent-runtime)** compact tool hint projection for routing and loop selection (by @[itscheems](https://github.com/itscheems)) - ([f39225e](https://github.com/itscheems/Roku/commit/f39225e2a76a03ad34460ac0bc63332bf8ffdc29))
- **(roku-cmd)** load agent runtime config during bootstrap (by @[itscheems](https://github.com/itscheems)) - ([21d0d88](https://github.com/itscheems/Roku/commit/21d0d88449429965ab8e7e0058bf982abfdcc7e3))
- **(roku-cmd)** load plugin runtime config during bootstrap (by @[itscheems](https://github.com/itscheems)) - ([cb9b13f](https://github.com/itscheems/Roku/commit/cb9b13f747e232b1acd2443e99c9523bec15d435))
- **(roku-plugin-tools)** sharpen builtin tool boundary hints (by @[itscheems](https://github.com/itscheems)) - ([bef92a8](https://github.com/itscheems/Roku/commit/bef92a8eb901bd12335d4b3e0f8dd3d916cdc995))
- **(roku-router)** shrink route classification back to coarse hints (by @[itscheems](https://github.com/itscheems)) - ([356a6e6](https://github.com/itscheems/Roku/commit/356a6e696d29ca0c66f4d02fa764c9ccd2f6c9c0))
- **(roku-runtime)** remove static follow-up routing from the generic loop (by @[itscheems](https://github.com/itscheems)) - ([1280ebd](https://github.com/itscheems/Roku/commit/1280ebd12a91af96dceaf6b070b35099ac201f09))
- **(roku-runtime)** tighten loop pause and visibility contracts (by @[itscheems](https://github.com/itscheems)) - ([0290f2c](https://github.com/itscheems/Roku/commit/0290f2c21635ffd73202c528e28060f8afdff675))
- **(roku-runtime)** remove filesystem loop shadow paths (by @[itscheems](https://github.com/itscheems)) - ([6e88db0](https://github.com/itscheems/Roku/commit/6e88db06d96cd585f8929018ba9e846dea2e40a9))
- **(roku-runtime)** add soft cross-tool routing to the generic loop (by @[itscheems](https://github.com/itscheems)) - ([aa0d3d7](https://github.com/itscheems/Roku/commit/aa0d3d7d73ee37398f1f21dfd7ea53f66b43969e))
- **(roku-runtime)** resume clarification through the generic loop (by @[itscheems](https://github.com/itscheems)) - ([9690c8f](https://github.com/itscheems/Roku/commit/9690c8fd5658ee2e5ffb1744f27f0e5ddd66a25e))
- **(roku-runtime)** treat route decisions as loop hints (by @[itscheems](https://github.com/itscheems)) - ([0e4f2e3](https://github.com/itscheems/Roku/commit/0e4f2e308ac14d7fa9d2ea28c3fba7e7c70be478))
- **(roku-runtime)** make tool loop observation-driven (by @[itscheems](https://github.com/itscheems)) - ([11ac54b](https://github.com/itscheems/Roku/commit/11ac54ba637ded27057b69ba41e3e8852cf80b71))
- **(roku-runtime)** formalize loop context contracts (by @[itscheems](https://github.com/itscheems)) - ([8e10bfd](https://github.com/itscheems/Roku/commit/8e10bfd5df0b0031a88c1d1204a2d3f4cddee227))
- **(roku-runtime)** make runtime mode explicit across runtime entrypoints (by @[itscheems](https://github.com/itscheems)) - ([490e848](https://github.com/itscheems/Roku/commit/490e8488aaba1a29cd80b122a4ae59927b8710ea))

### Documentation

- **(roku-plugin-tools)** clarify filesystem output limits (by @[itscheems](https://github.com/itscheems)) - ([c769c33](https://github.com/itscheems/Roku/commit/c769c334c6b0862f773e80d82049321e77710a02))

### Testing

- **(roku-runtime)** refresh route hint expectations (by @[itscheems](https://github.com/itscheems)) - ([849e681](https://github.com/itscheems/Roku/commit/849e681602db6b8351e63a55fdbea7891e6a9827))
- **(roku-runtime)** refresh react loop regression fixtures (by @[itscheems](https://github.com/itscheems)) - ([80578f1](https://github.com/itscheems/Roku/commit/80578f18eb41a915159df91ace200d5d4f5040a9))
- **(roku-runtime-service)** align pending loop recovery with explicit next-step contracts (by @[itscheems](https://github.com/itscheems)) - ([7294b2e](https://github.com/itscheems/Roku/commit/7294b2e7a2201285b1ddbe0ce0179b8002901d3a))

## [v0.0.3] - 2026-03-12

### Features

- **(roku-plugin-tools)** add filesystem grounding support (by @[itscheems](https://github.com/itscheems)) - ([59f6495](https://github.com/itscheems/Roku/commit/59f6495b92ad1dba533ee383d71ba696aaa3103d))
- **(roku-runtime)** extend loop execution across direct families (by @[itscheems](https://github.com/itscheems)) - ([7c3f22b](https://github.com/itscheems/Roku/commit/7c3f22be887439ce1cf4aae49c3cc0f6710950b9))
- **(roku-runtime-service)** execute filesystem requests through loop (by @[itscheems](https://github.com/itscheems)) - ([ae211a2](https://github.com/itscheems/Roku/commit/ae211a2ee05384b8a1b8295a12883a10e193d40b))
- **(roku-telegram)** persist pending loop recovery in session state (by @[itscheems](https://github.com/itscheems)) - ([ea0e7fa](https://github.com/itscheems/Roku/commit/ea0e7fa5e080972fe1a122e36bdd6e9c19be8646))

### Refactor

- **(roku-agent-runtime)** remove stale route scratchpad (by @[itscheems](https://github.com/itscheems)) - ([d724396](https://github.com/itscheems/Roku/commit/d72439616310e6f05297c960e1e6bfc0d3450db3))
- **(roku-agent-runtime)** cut direct routes over to runtime loop (by @[itscheems](https://github.com/itscheems)) - ([9fc3222](https://github.com/itscheems/Roku/commit/9fc3222d044fb9b3000e3ecbb663f7edd03a176f))
- **(roku-agent-runtime)** move fs next-step logic into runtime loop (by @[itscheems](https://github.com/itscheems)) - ([741f0ba](https://github.com/itscheems/Roku/commit/741f0ba77e5bc5598b8df3f962e77ac2c0a17621))
- **(roku-agent-runtime)** add runtime loop contracts (by @[itscheems](https://github.com/itscheems)) - ([a79d7ff](https://github.com/itscheems/Roku/commit/a79d7ff955a376a7c5b511ce22cb36d5c23a432a))
- **(roku-runtime-service)** remove transitional loop bridge (by @[itscheems](https://github.com/itscheems)) - ([3940115](https://github.com/itscheems/Roku/commit/394011507eb43a46bf74f77e3d7de2306c138fd9))
- **(roku-runtime-service)** add runtime loop bridge (by @[itscheems](https://github.com/itscheems)) - ([3b79923](https://github.com/itscheems/Roku/commit/3b7992367ce55850f19c4da83fea729dfe56f745))

### Documentation

- **(roku-plugins)** clarify catalog descriptor usage (by @[itscheems](https://github.com/itscheems)) - ([088fce2](https://github.com/itscheems/Roku/commit/088fce2e56aa2f1cfd32983dc0d0abf453415ec0))

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
