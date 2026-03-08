# Roku Agent Implementation Todo List

更新时间：2026-03-08

## 文档定位

- `tmp/agent-design-doc.md` 是架构白皮书，定义系统设计方向、分层和模块边界。
- `docs/todo-list.md` 是实施路线图，按 crate 粒度拆解为可执行子任务。
- 本文档优先按设计文档的分层顺序组织：基础工程 -> 控制面 / 认知层 -> 执行面 -> 数据面 -> 接入层 -> 组织层 -> 领域扩展 -> 测试矩阵。
- 表格中的状态只使用 `DONE` / `TODO`。
- `DONE` 表示当前仓库里已有相应实现，并且已纳入现有开发主线；`TODO` 表示尚未完成或仅有很薄的骨架。
- 带 `(planned)` 的 crate 标题表示该 crate 已被设计文档明确需要，但当前尚未独立建立或尚未进入 workspace。

## workspace

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| WS-01 | 统一所有 crate 命名为 `roku-xxx` 并完成 workspace 成员注册 | §19 Rust 工程结构 | DONE |
| WS-02 | 固化约定式提交 scope 为 `roku-xxx` 命名规范 | 项目规范 / 实施流程 | DONE |
| WS-03 | 维护 `Cargo.toml` workspace 依赖与基础包元信息 | §19 Rust 工程结构 | DONE |
| WS-04 | 增加统一的 lint / test / fmt 自动化入口（如 `just` / CI） | §20 测试与验收 | DONE |
| WS-05 | 增加 release profile、bench profile 与 workspace 级构建优化策略 | §5 非功能基线 | TODO |
| WS-06 | 增加 deploy / docker / k8s 工程骨架，与设计文档的部署结构对齐 | §19 Rust 工程结构 | TODO |
| WS-07 | 增加开发态 service orchestration 入口（`start-all` / `stop-all` / `doctor`）并为后续常驻组件扩展保留 registry | §19 Rust 工程结构 / 运行治理 | DONE |

## roku-common-types

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| CT-01 | 定义 `TaskId` / `NodeId` / `RequestId` / `ApprovalId` / `ArtifactId` / `ExperimentRunId` 等基础标识 | §8 Task Graph / §12 Artifact | DONE |
| CT-02 | 定义 `RequestEnvelope` / `ResponseEnvelope` / `ResponseStatus` 契约 | §9 请求生命周期 | DONE |
| CT-03 | 定义 `Task` / `TaskState` / `TaskEvent` / `ErrorClass` 等编排契约 | §8 编排状态机 | DONE |
| CT-04 | 定义 `PlanOutline` / `PlanStep` / `TaskGraph` / `TaskNode` / `TaskEdge` 等规划与执行图契约 | §7 Planning Architecture / §8 Task Graph | DONE |
| CT-05 | 定义 `JoinPolicy` / `AggregationMode` / `NodeResultSet` 以支持分支汇合语义 | §8.1 / §8.2 | DONE |
| CT-06 | 定义 `AgentContext` / `PolicyBindings` / `AgentInstanceSpec` 以支撑 capability-based agent model | §7.6 / §7.7 | DONE |
| CT-07 | 定义 `CapabilityToken` 基础 DTO | §10 Capability 模型 | DONE |
| CT-08 | 定义 `ResultEnvelope` / `EvidenceItem` / `ValidationEvidenceSet` / `ValidationReport` 契约 | §11 结构化结果保障 | DONE |
| CT-09 | 定义 `Artifact` / `ArtifactMetadataEntry` 契约 | §12 Artifact 设计 | DONE |
| CT-10 | 定义 `ExperimentRun` / `ExperimentMetric` / `ExperimentStatus` 契约 | §12 Artifact / Experiment | DONE |
| CT-11 | 定义 `CodingWorkContract` / `CodeChangeReport` 以承接外部 Coding Provider | §13.4 外部 Coding MCP Provider | DONE |
| CT-12 | 将 `ResultEnvelope.payload` 从纯字符串升级为 typed / structured payload 承载模型 | §11.1 结果合同 | TODO |
| CT-13 | 引入 schema descriptor / version compatibility DTO，避免 schema 逻辑散落在各 crate | §11.2 Schema Validation | TODO |
| CT-14 | 增加 A2A 所需的 `AgentIdentity` / `CapabilityCard` / `WorkContract` / `DelegationTicket` DTO | §17 组织层抽象 | TODO |
| CT-15 | 增加组织级策略、审批规则、预算快照、模型路由等跨 crate 通用契约 | §14 / §15 / §17 | TODO |
| CT-16 | 增加 `PlanningModeHint` / `SessionPreferences` / `ConversationTurn` 等会话级策略与记忆契约 | §7 Planning Architecture / §12 Memory | DONE |

## roku-orchestrator

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| OR-01 | 创建任务实体并初始化 `Queued` 状态 | §8.3 状态机 | DONE |
| OR-02 | 实现显式状态跃迁校验，阻断非法状态转换 | §8.4 状态变更要求 | DONE |
| OR-03 | 实现失败注册、重试预算与 dead-letter 终态流转 | §8.4 状态变更要求 | DONE |
| OR-04 | 输出幂等键构造函数 `task_id + node_id + attempt` | §8.4 状态变更要求 | DONE |
| OR-05 | 增加基于持久化事件的任务重建 / replay 能力 | §8 / §20.2 验收重点 | TODO |
| OR-06 | 增加取消、补偿与中断恢复状态流 | §8 状态机 / §22 风险缓解 | DONE |
| OR-07 | 增加 node-level deadline / budget snapshot enforcement | §8.4 / §14 预算治理 | TODO |
| OR-08 | 接入真实 dispatch plane（Redis / NATS / JetStream）后的 lease / ack / retry 协议 | §14.3 背压与资源隔离 | TODO |

## roku-supervisor-agent (planned)

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| SA-01 | 拆出独立 `Supervisor Agent` crate，承接高层目标理解与策略控制 | §4.1 平台抽象 / §7.2 | DONE |
| SA-02 | 建立目标、约束、预算、风险偏好的规范化输入模型 | §7.2 / §14 | DONE |
| SA-03 | 将重规划策略从 `runtime-service` 中抽离为显式 supervisor policy | §7.2 / §7.5 | DONE |
| SA-04 | 实现最终结果聚合与完成判定策略 | §7.2 / §9 请求生命周期 | TODO |
| SA-05 | 实现 supervisor 对 reviewer / research / data / coding provider 的委派规则 | §7.2 / §7.7 / §13.4 | TODO |
| SA-06 | 实现 supervisor 与组织策略、审批、预算治理的绑定 | §10 / §14 / §15 / §17 | TODO |

## roku-planning-engine

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| PE-01 | 实现 `ReAct` / `TaskDecomposition` / `TreeSearch` / `IterativeRefinement` 四种 planning mode | §7.3 Planning Strategy 模式 | DONE |
| PE-02 | 实现复杂度 / 不确定性 / 风险 / 预算驱动的策略选择 | §7.4 Strategy 选择规则 | DONE |
| PE-03 | 增加 `PlanningHook`、`PlanningLoopState`、`PlanningStopReason` 等 loop 控制合同 | §7.2 / §7.3 | DONE |
| PE-04 | 限制最大轮次、分支预算与 token 预算，避免无界 planner loop | §8.4 状态变更要求 | DONE |
| PE-05 | 将执行期反馈（validation failure、timeout、tool denial）纳入策略再选择 | §7.4 / §22 风险缓解 | TODO |
| PE-06 | 增加 tree-search 分支评分、剪枝和候选排序接口 | §7.3 Tree Search | TODO |
| PE-07 | 增加 planning stop reason 的可观测输出与质量指标采集 | §16 指标 / §20 测试与验收 | TODO |
| PE-08 | 接入 `roku-llm-adapter`，让 planning 调用受预算 / 风险 / provider policy 控制 | §3 规划与执行分离 / §14 预算治理 | TODO |

## roku-task-planner

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| TP-01 | 让 planner 消费完整 `PlanningDecision` 而非单一 mode 枚举 | §7.2 / §7.5 | DONE |
| TP-02 | 为四种 planning mode 生成 mode-aware `PlanOutline` | §7.3 Planning Strategy 模式 | DONE |
| TP-03 | 在 `PlanStep` 中输出显式依赖 `depends_on` | §7.5 / §8.1 | DONE |
| TP-04 | 在 decomposition / tree-search / refinement 中生成 branch / merge / critique loop 结构 | §7.3 / §8.2 | DONE |
| TP-05 | 为每个 step 补充 acceptance criteria、预期 schema 和失败重试提示 | §7.2 / §11 结构化结果 | TODO |
| TP-06 | 增加可复用 planner template library，支持高频任务类型模板化 | §7.4 历史表现 | TODO |
| TP-07 | 接入 memory / artifact / experiment 检索以辅助 plan outline 生成 | §12 Context、Memory 与 Artifact | TODO |
| TP-08 | 为 quant / coding / review 类任务增加特定 outline 生成模板 | §4.2 适用场景 / §18 量化研究 Agent | TODO |
| TP-09 | 对简单对话型 `ReAct` 请求降级为单步 direct-action outline，减少不必要的观察步骤与 live LLM 波动 | §7.3 ReAct / §22 风险缓解 | DONE |

## roku-execution-graph-builder

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| GB-01 | 实现 `PlanOutline -> TaskGraph` 编译主链路 | §7.2 / §8.1 | DONE |
| GB-02 | 基于 `depends_on` 编译依赖边而非简单线性串联 | §7.5 / §8.2 | DONE |
| GB-03 | 检测重复 step、缺失依赖等 graph build 错误 | §8.4 / §20 验收重点 | DONE |
| GB-04 | 让 validation gate 依赖于 terminal steps，支持分支场景 | §8.2 并行分支 / §11 验证流水线 | DONE |
| GB-05 | 提供 ready-node scheduler、执行层次划分与 cycle detection | §8 Task Graph / 调度 | DONE |
| GB-06 | 为 graph builder 注入 recovery point / resume point / partial rerun anchor | §8.1 ResumePoint | DONE |
| GB-07 | 自动注入 approval / aggregation / retry / dead-letter 辅助节点 | §8.2 / §15.3 人审闸门 | TODO |
| GB-08 | 增加条件边、条件分支和受控回环 DAG 编译能力 | §8.2 条件分支 / 回环有限图 | TODO |
| GB-09 | 为每个节点写入预算、deadline、capability requirement snapshot | §8.4 / §14 | DONE |

## roku-agent-instance-factory

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| AF-01 | 建立 `research` / `data` / `review` / `general` 默认 capability profile | §7.7 默认 Profile | DONE |
| AF-02 | 支持自定义 `CapabilityProfile` 注册 | §7.6 Capability-based Agent Model | DONE |
| AF-03 | 实现 profile capability + node capability 合并 | §7.6 / §7.7 | DONE |
| AF-04 | 根据 capability prefix 推断最合适的 profile | §7.7 / §7.8 | DONE |
| AF-05 | 根据 node 复杂度推导 `PolicyBindings` 中的 token / time budget | §7.6 / §14 | DONE |
| AF-06 | 将 working context 精简为最小必需上下文、artifact refs、memory refs | §7.6 Context / §12 分层记忆 | TODO |
| AF-07 | 让 profile 装配显式绑定审批策略、模型路由策略和验证策略 | §7.6 PolicyBindings | TODO |
| AF-08 | 支持带外部 coding provider 的组合 worker profile | §7.7 / §13.4 | TODO |
| AF-09 | 增加组织级 profile 模板与租户级 policy override | §17 OrgPolicy | TODO |

## roku-agent-runtime

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| AR-01 | 建立 capability-dispatched worker registry | §7.2 Agent Instance / Execution Worker | DONE |
| AR-02 | 提供内建 `research-worker` / `data-worker` / `review-worker` / `generic-worker` | §7.7 默认 Profile | DONE |
| AR-03 | 基于 `PolicyBindings` 做最小预算拒绝保护 | §14 预算传播规则 | DONE |
| AR-04 | 支持动态注册自定义 worker | §7.8 可演进性 | DONE |
| AR-05 | 将 worker 执行真实接入 `roku-tool-runtime` 而非合成结果 | §9 请求生命周期 | DONE |
| AR-06 | 将复杂 reasoning worker 接入 `roku-llm-adapter` | §3 规划与执行分离 / §7 | DONE |
| AR-07 | 按 profile / task type 选择输出 schema 和 evidence 模板 | §11 结果合同 | TODO |
| AR-08 | 将 timeout / retry / budget 消耗下放到 worker 执行层 | §14 预算与超时 | TODO |
| AR-09 | 在 live worker prompt 中注入可信 runtime date/time context，并显式抑制 meta-reasoning 泄漏 | §7 Agent Instance / §16 可观测与运行治理 | DONE |

## roku-llm-adapter

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| LLM-01 | 定义 `RiskTier` / `ModelProfile` / `RoutingPolicy` / `GenerationRequest` / `LlmResponse` | §14 预算模型 / §22 风险缓解 | DONE |
| LLM-02 | 定义 `LlmProvider` 抽象与 provider response 契约 | §3 扩展优先 / §19 llm-adapter | DONE |
| LLM-03 | 基于 context window、token budget、cost budget、risk tier 过滤可用模型 | §7.4 / §14.1 | DONE |
| LLM-04 | 对高风险请求优先选择更强模型，对低风险请求优先低成本模型 | §7.4 / §22 风险缓解 | DONE |
| LLM-05 | 对 provider 响应做 latency 与预算二次守卫 | §5 非功能基线 / §14 | DONE |
| LLM-06 | 增加 provider 级 retry / backoff / circuit breaker | §22 风险与缓解 | DONE |
| LLM-07 | 将 provider 选择、token / cost 消耗输出到 observability | §16 指标 | DONE |
| LLM-08 | 将 adapter 真正接入 planning / runtime / coding provider 选择链路 | §4.1 / §7.5 | TODO |
| LLM-09 | 增加 deterministic request key / prompt cache / replay 支持 | §20 回归测试 / replay | TODO |
| LLM-10 | 增加 OpenRouter provider、环境变量装配、默认主模型链与兼容头部 | §4.1 / §19 llm-adapter | DONE |
| LLM-11 | 将 `system/user` 消息显式映射到 OpenAI-compatible chat completions schema，并支持 OpenRouter `models[]` fallback chain | §4.1 / §19 llm-adapter | DONE |
| LLM-12 | 对 OpenRouter 请求显式关闭 reasoning surfacing，并在响应解析时只接受 assistant content / refusal，拒绝将 reasoning 当作最终答案 | §19 llm-adapter / live response safety | DONE |
| LLM-13 | 对 `content=null` / `finish_reason=length` 等不可读 OpenRouter 响应执行显式 fallback model 重试，并按模型特性调整 reasoning 请求参数 | §19 llm-adapter / §22 风险缓解 | DONE |

## roku-capability-auth

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| CA-01 | 实现 capability token 签发 | §10.4 Capability Authority | DONE |
| CA-02 | 实现 capability attenuation，保证子 token 权限不扩张 | §10.1 / §10.6 | DONE |
| CA-03 | 实现 action + expiry 校验 | §10.2 / §10.5 | DONE |
| CA-04 | 记录本地 audit log，保留签发 / 校验 / 衰减事件 | §10.4 / §15.2 | DONE |
| CA-05 | 增加 delegation chain、issuer、subject、resource selector 的结构化模型 | §10.2 Capability 模型 | TODO |
| CA-06 | 增加路径白名单、网络白名单、模型等级、预算上限等约束校验 | §10.3 Capability 维度 | TODO |
| CA-07 | 增加 token revocation、policy hot reload 和 deny reason 分类 | §10.4 Capability Authority | TODO |
| CA-08 | 与审批系统联动，对高风险动作启用 approval-aware issuance | §10.5 / §15.3 | TODO |

## roku-validation-plane

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| VP-01 | 建立 schema / semantic / provenance / policy 四层校验流水线 | §11.2 四层校验模型 | DONE |
| VP-02 | 增加 `backtest_report.v1` 的语义校验 baseline | §11.2 Semantic Validation | DONE |
| VP-03 | 要求 evidence 存在并校验 artifact ownership / node ownership | §11.2 Provenance Validation | DONE |
| VP-04 | 对低置信度结果进行 policy 拒绝 | §11.2 Policy Checker | DONE |
| VP-05 | 支持 artifact-backed evidence set 校验 | §11.3 验证流水线 | DONE |
| VP-06 | 接入 schema registry，支持 schema version 兼容与字段级错误提示 | §11.2 Schema Validation | TODO |
| VP-07 | 增加基础 cross-check，覆盖 artifact 去重、schema 一致性与异常置信度校验 | §11.2 Cross-Check | DONE |
| VP-08 | 增加 reviewer agent / alternate worker escalation path | §11.4 失败处理策略 | TODO |
| VP-09 | 增加 `untrusted result` 隔离状态，不允许其进入最终聚合 | §11.4 / §11.5 | TODO |
| VP-10 | 增加 independent verification / 双执行复核链路，支持独立结果比对与分歧升级 | §11.2 Cross-Check | TODO |

## roku-tool-runtime

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| TR-01 | 定义 `ToolDescriptor` / `ToolSchema` / `RuntimeConstraints` / `SandboxProfile` | §13.1 Tool Descriptor | DONE |
| TR-02 | 对输入进行 required-field schema 校验 | §11 Schema Validation / §13.1 | DONE |
| TR-03 | 对 required capabilities 进行 invoke 前检查 | §10 Capability Layer | DONE |
| TR-04 | 实现 timeout / bounded retry / backoff policy | §13.1 / §14 | DONE |
| TR-05 | 输出 deterministic execution hooks 和 output fingerprint | §13 / §16 Trace 与日志 | DONE |
| TR-06 | 维护 descriptor-based registry，而非字符串回调表 | §13 Tool / Skill / MCP | DONE |
| TR-07 | 接入真实 sandbox executor（WASI / container / restricted process） | §6.1 Sandbox Executor / §15.1 | TODO |
| TR-08 | 对工具输出做 output schema validation 与结构化 observation 映射 | §11 / §13.1 | TODO |
| TR-09 | 对工具调用做预算记账、artifact 化和审计事件持久化 | §14 / §15 / §16 | TODO |
| TR-10 | 接入 skill registry / MCP discovery，支持 tool catalog 与版本治理 | §13.2 / §13.3 | TODO |
| TR-11 | 将 `roku-tool-runtime` 拆分为 descriptor / error / event / runtime / tests 模块，降低单文件复杂度 | §13 Tool / Skill / MCP | DONE |

## roku-skill-registry (planned)

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| SR-01 | 建立 `SkillDescriptor` / version / signature / dependency 模型 | §13.2 Skill 生命周期 | TODO |
| SR-02 | 实现 skill 注册、验证、发布、回滚主链路 | §13.2 | TODO |
| SR-03 | 对 skill 做 schema / capability / dependency / smoke test 审查 | §13.2 | TODO |
| SR-04 | 支持租户级 / 版本级灰度发布 | §13.2 发布 | TODO |
| SR-05 | 增加 success rate / failure rate / permission deny rate 统计与熔断 | §13.2 观测 / 回滚 | TODO |
| SR-06 | 接入 `roku-tool-runtime`，让 skill 成为受治理的执行资源 | §13 Tool / Skill / MCP | TODO |

## roku-mcp-bridge

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| MCP-01 | 建立 MCP request / response 协议边界 | §13.3 MCP 集成边界 | DONE |
| MCP-02 | 提供最小 `NoopMcpBridge`，用于本地协议占位 | 集成基线 | DONE |
| MCP-03 | 增加远端 tool discovery 与 descriptor 映射 | §13.3 / §6.1 MCP Bridge | DONE |
| MCP-04 | 强制本地 capability 决策高于远端 MCP tool 声明 | §13.3 / §10 | TODO |
| MCP-05 | 增加 session lifecycle、retry taxonomy、transport recovery | §13.3 / §22 风险缓解 | TODO |
| MCP-06 | 输出 trace / metrics / audit hooks，并与 observability 对接 | §16 可观测性 | TODO |

## roku-coding-provider-adapter

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| CPA-01 | 建立 `CodingProvider` 抽象 | §13.4 外部 Coding MCP Provider 设计 | DONE |
| CPA-02 | 提供 `McpCodingProvider` 以通过 MCP 发起编码任务 | §13.4 | DONE |
| CPA-03 | 输出最小 `CodeChangeReport` 结构化结果 | §13.4 code_change_report.v1 | DONE |
| CPA-04 | 让 contract 包含 allowed paths、acceptance checks、budget 约束并传给 provider | §13.4 coding_work_contract | DONE |
| CPA-05 | 规范化 provider 的命令执行结果、测试结果、残余风险 | §13.4 最小输出 | DONE |
| CPA-06 | 增加 provider 不可用 / 超预算 / 校验失败时的 degrade-to-advice 模式 | §13.4 适用原则 | TODO |
| CPA-07 | 接入 capability attenuation，限制 provider 目录、命令与网络权限 | §10 / §13.4 | TODO |
| CPA-08 | 将 provider 输出接入 validation-plane 与 artifact-store | §11 / §13.4 | TODO |

## roku-api-gateway

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| AG-01 | 提供 `/health` 与 `/v1/requests` HTTP 路由 | §19.1 网关实现细节 | DONE |
| AG-02 | 将 HTTP submit 绑定到 `RuntimeServiceExecutor` | §9 请求生命周期 | DONE |
| AG-03 | 提供审批查询 / 审批决策路由 | §15.3 人审闸门 | DONE |
| AG-04 | 提供 task artifacts / experiment 查询路由 | §12 Artifact / Experiment | DONE |
| AG-05 | 提供 artifact content / download 路由 | §12 Artifact / §19.1 | DONE |
| AG-06 | 为 approval / artifact 路由增加更细粒度错误映射 | §19.2 错误模型 | DONE |
| AG-07 | 增加 auth / rate limit / idempotency middleware | §4.1 Gateway / §14.3 | TODO |
| AG-08 | 增加 request-id / trace-id 透传与全链路 correlation | §16 Trace 与日志 | TODO |
| AG-09 | 增加异步任务观察、流式响应或 callback 机制 | §9 请求生命周期 / 长任务治理 | TODO |
| AG-10 | 增加 OpenAPI / schema 文档与 API versioning 策略 | §19.1 / §20 验收 | TODO |
| AG-11 | 在 `roku-cmd` 中补齐 `api-gateway` 常驻服务启动入口，并纳入开发态 service registry | §4.1 Connector / Gateway 运行形态 | DONE |
| AG-12 | 增加 `task replay` 诊断路由，并直接复用 `roku-runtime-service` 的 replay report 查询接口 | §8 ResumePoint / §19.1 Gateway 运维接口 | DONE |

## roku-connectors-telegram

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| TG-01 | 建立 Telegram inbound update model 与 serde 映射 | §4.1 Connector / Telegram 接入 | DONE |
| TG-02 | 实现 inbound normalization，将 Telegram 消息转为 runtime 请求 | §9 请求生命周期 | DONE |
| TG-03 | 实现 outbound message formatting，将 runtime 响应映射为 Telegram 文本 | §4.1 Connector | DONE |
| TG-04 | 显式拒绝 bot-originated message 等不合法输入 | 边界治理 | DONE |
| TG-05 | 增加真实 webhook / polling transport runner | §4.1 Connector | DONE |
| TG-06 | 增加 approval callback query 支持 | §15.3 人审闸门 | DONE |
| TG-07 | 增加 artifact / experiment / approval 的富消息渲染 | §12 Artifact / §16 诊断 | DONE |
| TG-08 | 增加长任务状态推送与结果回执能力 | 长任务体验 | DONE |
| TG-09 | 为 polling transport 增加抑制式错误日志，隐藏 routine `getUpdates` 噪声并保留连续失败告警 | §16 可观测性 / Connector 运行治理 | DONE |
| TG-10 | 增加大小写无关的 `/react` `/taskdecomposition` `/treesearch` `/iterativerefinement` `/auto` 会话级策略命令 | §7.3 Planning Strategy 模式 / Telegram 交互 | DONE |
| TG-11 | 让 Telegram 请求携带 session-level planning override 与 recent conversation history | §9 请求生命周期 / §12 Memory | DONE |
| TG-12 | 默认仅向 Telegram 用户展示最终 answer 文本；request/status/artifact 元信息改为可配置扩展输出 | §4.1 Connector / Telegram 交互体验 | DONE |
| TG-13 | 将 progress notice 默认降到日志侧，通过环境变量显式开启用户可见进度提示 | §4.1 Connector / 长任务体验 | DONE |
| TG-14 | 支持单条命令式输入（如 `/react 你好`），并兼容原有两步式会话策略切换 | §7.3 Planning Strategy 模式 / Telegram 交互 | DONE |
| TG-15 | 对泄漏出的 prompt / analysis 文本做最终回复级清洗，并确保这类回复不会再污染会话记忆 | §4.1 Connector / §12 Memory / user response safety | DONE |
| TG-16 | 增加会话级多轮回归 harness，覆盖 planning-mode 切换后时间问题与后续知识问答的记忆污染场景 | §12 Memory / §20 集成测试 | DONE |

## roku-state-store

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| SS-01 | 定义 `TaskRepository` / `EventRepository` / `ApprovalRepository` / `ResultRepository` trait | §6.2 数据面 / §19 state-store | DONE |
| SS-02 | 提供 task / event / approval / result 的 in-memory adapter | §21 Phase 0-1 基础骨架 | DONE |
| SS-03 | 提供 task / event / approval / result 的 file-backed adapter | 原型持久化基线 | DONE |
| SS-04 | 为 task / approval / result 增加 roundtrip 测试 | §20 测试策略 | DONE |
| SS-04A | 增加 `SessionPreferenceRepository` / `ConversationRepository` trait 与 in-memory adapter | §12 Memory / §19 state-store | DONE |
| SS-04B | 增加 session preference / conversation history 的 file-backed adapter | §12 Memory / 原型持久化基线 | DONE |
| SS-05 | 增加 PostgreSQL task backend | §6.1 PostgreSQL / §21 Phase 4+ | DONE |
| SS-06 | 增加 PostgreSQL event / approval / result backend | §6.1 PostgreSQL | DONE |
| SS-06A | 增加 PostgreSQL session preference / conversation memory backend | §6.1 PostgreSQL / §12 Memory | DONE |
| SS-07 | 增加 migration / bootstrap / repository index 设计 | 生产级持久化边界 | TODO |
| SS-08 | 增加 Redis / NATS / JetStream 风格 dispatch 抽象 | §6.1 NATS JetStream / §14.3 | DONE |
| SS-09 | 增加 ack、lease、backpressure、retry claim 语义 | §14.3 背压与资源隔离 | DONE |
| SS-10 | 增加 replay / snapshot / compaction 能力，以支撑大规模任务恢复 | §8 ResumePoint / §20 replay | TODO |

## roku-artifact-store

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| AS-01 | 建立 artifact metadata repository 与 service 边界 | §12.2 Artifact 作为一等公民 | DONE |
| AS-02 | 将 node result 持久化为 artifact metadata | §12.2 / §9 请求生命周期 | DONE |
| AS-03 | 提供按 `artifact_id` / URI / task 查询 artifact 的能力 | §12.3 实体建议 | DONE |
| AS-04 | 持久化 artifact 内容并支持 content lookup | §12 Artifact / §19 artifact-store | DONE |
| AS-05 | 对 file-backed repository 做向后兼容快照升级 | 工程可演进性 | DONE |
| AS-06 | 将 artifact 内容抽象为 blob/object store，而不是仅存字符串 payload | §12.2 / 生产数据面 | TODO |
| AS-07 | 支持 media type、binary payload、size、checksum 强校验 | §12.3 Artifact 实体 | TODO |
| AS-08 | 支持 retention / archive / encryption / access policy | §15 安全边界 / 数据治理 | TODO |
| AS-09 | 支持 artifact lineage / parent-child references / provenance graph | §12 Artifact 引用关系 | TODO |

## roku-experiment-registry

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| ER-01 | 建立 experiment run repository 与 registry service | §12.2 / §18 量化研究 Agent | DONE |
| ER-02 | 提供 start / attach artifact / complete / fail 生命周期 | §12.3 ExperimentRun | DONE |
| ER-03 | 支持 in-memory / file-backed experiment persistence | 原型持久化基线 | DONE |
| ER-04 | 输出 summary / metrics / artifact association | §12.3 ExperimentRun | DONE |
| ER-05 | 增加数据版本、代码版本、模型版本等 reproducibility metadata | §12.2 / §18.3 关键校验 | TODO |
| ER-06 | 增加按任务、策略、时间范围、状态的查询接口 | 数据面可检索性 | TODO |
| ER-07 | 增加 run 对比、lineage 与基准回归能力 | §18 量化研究 Agent | TODO |

## roku-observability

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| OB-01 | 建立 requests / failures / validation / approvals / dead-letter 基础计数器 | §16.1 指标 | DONE |
| OB-02 | 建立 artifact / experiment telemetry | §16.1 指标 | DONE |
| OB-03 | 建立 planning strategy metrics | §16.1 Planning 指标 | DONE |
| OB-04 | 建立 `AuditSink` / `InMemoryAuditSink` / `JsonlAuditExporter` | §15.2 审计模型 | DONE |
| OB-05 | 为审计记录增加 `AuditCorrelation` 与 attribute tags | §16.2 Trace 与日志 | DONE |
| OB-06 | 将 runtime 中的 capability denial、validation accept、approval decision 接入 audit correlation | §15.2 / §16.2 | DONE |
| OB-07 | 增加 OTEL / Prometheus exporter | §6.1 Observability / §16 | TODO |
| OB-08 | 增加 planning stop reason、loop depth、branch count 直方图 | §16 Planning 指标 | TODO |
| OB-09 | 增加 tool / provider cost、latency、timeout、deny-rate 指标 | §16 Tool / Budget / Validation 指标 | TODO |
| OB-10 | 增加外部 SIEM / audit sink 对接 | §15.2 审计模型 | TODO |
| OB-11 | 增加可配置的全局 `LogSink` 抽象与 fanout 组合 | §16 Trace 与日志 / 工程化 | DONE |
| OB-12 | 增加异步滚动文件日志 sink，默认按 `logs/<component>/<timestamp>.log` 落盘 | §16 Trace 与日志 / Connector 运行治理 | DONE |
| OB-13 | 将开发态 service stdout 日志改为按启动时间命名，并让 doctor 展示最新日志路径 | §16 Trace 与日志 / 工程化 | DONE |

## roku-runtime-service

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| RS-01 | 建立可复用 runtime orchestration service 入口 | §4.1 Runtime Layer / §9 请求生命周期 | DONE |
| RS-02 | 实现 planning -> graph building -> delegation -> execution -> validation -> aggregation 主链路 | §9 请求生命周期 | DONE |
| RS-03 | 将 CLI / HTTP 执行入口统一复用该 service | §4.1 Connector / Runtime Layer | DONE |
| RS-04 | 持久化 approval ticket，并支持 `get_approval` / `decide_approval` / resume | §15.3 人审闸门 | DONE |
| RS-05 | 从线性执行升级为 dependency-aware DAG 调度 | §8.2 并行分支 / DAG | DONE |
| RS-06 | 用 result repository 和 artifact-backed evidence 代替单一 `last_result` 依赖 | §11 / §12 | DONE |
| RS-07 | 消费 `JoinPolicy` / `AggregationMode` / `NodeResultSet` 实现显式聚合与验证 | §8.1 / §11.3 | DONE |
| RS-08 | 接入 experiment registry 与 artifact store | §12 Artifact / Experiment | DONE |
| RS-09 | 对外暴露 artifact / experiment / artifact-content 查询 API | §12 / §19.1 | DONE |
| RS-10 | 将 planning metrics 与 correlated audit 接入 observability | §16 指标 / Trace | DONE |
| RS-11 | 将 node execution 真正下放到 `roku-tool-runtime`，形成受 descriptor 约束的执行链 | §9 请求生命周期 / §13 | DONE |
| RS-12 | 将 planning / reasoning 接入 `roku-llm-adapter`，消除硬编码 planning 输入 | §7 Planning Architecture / §14 | DONE |
| RS-12A | 支持 request-level / session-level planning mode override，并补齐四种 planning mode 测试覆盖 | §7.3 Planning Strategy 模式 | DONE |
| RS-12B | 将 recent conversation history 注入 planner 与 worker prompt 构建链路 | §12 Memory / §7 Planning Architecture | DONE |
| RS-13 | 将 `Supervisor Agent` 逻辑从 `runtime-service` 中进一步显式分离 | §4.1 / §7.2 | DONE |
| RS-14 | 增加组织级审批规则、风险动作 gating 与 capability/approval 联动 | §15.3 / §17 OrgPolicy | TODO |
| RS-15 | 增加基于持久化事件的 replay / recovery / partial rerun 主链路 | §8 ResumePoint / §20 replay | TODO |
| RS-16 | 增加 cancellation / compensation / timeout recovery 主链路 | §8 状态机 / §22 风险缓解 | DONE |
| RS-17 | 暴露 `task snapshot` 与 `task event timeline` 查询接口，为后续 replay / CLI 运维命令提供 recovery 基线 | §8 ResumePoint / §20 replay | DONE |
| RS-18 | 增加 `resume_task` 入口，基于 persisted task snapshot + graph + completed nodes 恢复可继续执行的任务，并对等待审批态返回明确挂起响应 | §8 ResumePoint / §15.3 人审闸门 | DONE |
| RS-19 | 暴露统一 `task replay report` 查询接口，收敛事件链一致性与 recoverable 判定，避免 CLI / Gateway 重复实现编排规则 | §8 ResumePoint / §19 运维接口 | DONE |

## roku-cmd

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| CMD-01 | 提供最小 CLI 执行入口并复用 `roku-runtime-service` | §4.1 Connector / CLI | DONE |
| CMD-02 | 提供 `Normal` / `MissingEvidence` / `CapabilityDenied` / `ApprovalRequired` / `RetryExhausted` 等 run mode | 测试与演练辅助 | DONE |
| CMD-03 | 增加查询 task / approval / artifact / experiment 的 CLI 子命令 | 运维与诊断 | DONE |
| CMD-04 | 增加 artifact download / approval decision / resume 等运维命令 | 运行期治理 | DONE |
| CMD-05 | 增加配置文件 / 环境变量 / profile 加载能力 | 工程化 | DONE |
| CMD-06 | 增加 `live-once` / `telegram-bot` 命令，打通 live model 与 Telegram 入口 | §4.1 Connector / CLI | DONE |
| CMD-07 | 增加日志目录、滚动策略、stderr 开关等环境变量配置并安装全局 logger | §16 可观测性 / 工程化 | DONE |
| CMD-08 | 增加 CLI 级 planning mode override 参数，便于本地验证四种 planning 策略 | §7.3 / 测试与演练辅助 | DONE |
| CMD-09 | 增加 `scripts/dev-services.sh`，统一管理开发态常驻服务的 pid、stdout log 与运行目录 | §19 Rust 工程结构 / 工程化 | DONE |
| CMD-10 | 在 `justfile` 中增加 `start-all` / `stop-all` / `doctor` / `start` / `stop` / `status` recipe | §19 Rust 工程结构 / 运维诊断 | DONE |
| CMD-11 | 将 `doctor` 升级为分组面板视图，区分常驻服务、内嵌组件、集成配置、日志与端点 | §19 Rust 工程结构 / 运维诊断 | DONE |
| CMD-12 | 将 `api-gateway` 收敛为可选接口层，默认不随 `start-all` 启动，但保留 `doctor` 可见性与手动启动能力 | §4.1 Gateway 运行形态 / 运维诊断 | DONE |
| CMD-13 | 让 live runtime bootstrap 优先装配 PostgreSQL-backed orchestration state store，并在日志中显式输出 backend 选择 | §6.1 PostgreSQL / §19 state-store / §16 可观测性 | DONE |
| CMD-14 | 增加 `task show <task-id>` 与 `approval show <approval-id>` CLI 命令，直接查询持久化 task snapshot / event timeline / approval ticket | §19 CLI 运维入口 / §8 ResumePoint | DONE |
| CMD-15 | 增加 `approval approve|reject <approval-id> --actor ... [--comment ...]` CLI 命令，支撑最小审批决策闭环 | §15.3 人审闸门 / CLI 运维入口 | DONE |
| CMD-16 | 为 live / stateful runtime 默认装配 file-backed artifact-store 与 experiment-registry，并提供路径级环境变量配置 | §12 Artifact / Experiment / §19 工程化 | DONE |
| CMD-17 | 增加 `artifact list|content|download` 与 `experiment show` CLI 命令，打通 artifact / experiment 运维查询链路 | §12 Artifact / Experiment / CLI 运维入口 | DONE |
| CMD-18 | 增加 `task replay <task-id>` CLI 命令，基于 persisted task snapshot + event timeline 生成一致性与 recoverable 报告 | §8 ResumePoint / §20 replay | DONE |
| CMD-19 | 让 `task replay` 直接复用 `roku-runtime-service` 的 replay report，而不是在 CLI 层自行重建状态链规则 | §19 CLI 运维入口 / 降耦 | DONE |

## roku-agent-directory (planned)

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| AD-01 | 建立 `AgentIdentity` / `CapabilityCard` / `WorkContract` / `DelegationTicket` 数据模型 | §17.2 组织层抽象 | TODO |
| AD-02 | 实现按 capability / schema / SLA / cost 的 agent discovery | §17.3 A2A 流程 | TODO |
| AD-03 | 实现 work contract 下发与 delegated task 跟踪 | §17.3 / §17.4 | TODO |
| AD-04 | 支持外部 coding provider 与内部 agent 的统一目录注册 | §17.2 / §13.4 | TODO |
| AD-05 | 支持共享 artifact 引用、信任域和组织级策略绑定 | §17.2 SharedArtifactRef / OrgPolicy | TODO |
| AD-06 | 增加版本兼容、停用管理和目录治理能力 | §17.4 设计要求 | TODO |

## roku-quant-domain-pack (planned)

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| QD-01 | 定义 `data.*` / `research.*` / `strategy.*` / `backtest.*` / `analysis.*` / `risk_review.*` capability profiles | §18.2 量化 Capability Profiles | TODO |
| QD-02 | 定义数据集快照、回测结果、分析报告等 artifact schema | §18.1 / §18.3 | TODO |
| QD-03 | 扩展 experiment registry，补充数据版本 / 参数版本 / 代码版本绑定 | §18.3 关键校验 | TODO |
| QD-04 | 为量化任务增加专用 semantic / policy / risk review 校验器 | §18.3 / §11 验证模型 | TODO |
| QD-05 | 为 task planner 增加 quant workflow template（data -> factor -> strategy -> backtest -> analysis） | §18.1 / §7.3 | TODO |
| QD-06 | 增加量化基准样例和 e2e acceptance scenario | §18 量化研究 Agent | TODO |

## roku-e2e

| ID | Subtask | Design Anchor | Status |
| --- | --- | --- | --- |
| E2E-01 | 覆盖 CLI happy path | §20 端到端测试 | DONE |
| E2E-02 | 覆盖 validation failure path | §11 失败处理策略 | DONE |
| E2E-03 | 覆盖 capability denied path | §10 Capability 最小权限 | DONE |
| E2E-04 | 覆盖 approval pending -> approve -> succeed roundtrip | §15.3 人审闸门 | DONE |
| E2E-05 | 覆盖 dead-letter failure path | §8.3 / §8.4 | DONE |
| E2E-06 | 覆盖 HTTP artifact / experiment / artifact-content / download 路由 | §12 / §19.1 | DONE |
| E2E-07 | 增加 property-based 状态迁移 / graph compilation / capability attenuation 测试 | §20 测试策略 | TODO |
| E2E-08 | 增加 chaos 测试：timeout、进程重启、重复投递、provider 失联 | §20 混沌测试 | TODO |
| E2E-09 | 增加 replay 测试：基于持久化 task/event/result/artifact 恢复执行 | §20 回归测试 / replay | DONE |
| E2E-10 | 增加 tool / MCP / coding provider / approval gate / backpressure 组合矩阵 | §20 集成测试 / 验收重点 | TODO |
