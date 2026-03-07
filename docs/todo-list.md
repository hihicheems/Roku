# Roku Agent Project Todo List

更新时间：2026-03-07

说明：
- 这是项目级 roadmap，不是开发流水日志。
- 状态仅使用：`todo` / `doing` / `done`。
- 任务按系统架构、crate 边界和功能链路拆分。

| ID | Crate / Module | Task | Status |
| --- | --- | --- | --- |
| 01 | `workspace` / `Cargo.toml` | 统一 `roku-xxx` crate 命名、workspace 结构和基础工程约束 | `done` |
| 02 | `roku-common-types` | 完成 request/task/result/approval/capability/artifact/experiment 基础契约定义 | `done` |
| 03 | `roku-orchestrator` | 完成任务状态机、失败分类、重试预算和 dead-letter 基础能力 | `done` |
| 04 | `roku-planning-engine` | 实现 planning mode 选择与基础策略决策接口 | `done` |
| 05 | `roku-task-planner` | 输出基础 `plan outline`，形成 task decomposition 入口 | `done` |
| 06 | `roku-execution-graph-builder` | 完成 baseline `plan outline -> TaskGraph` 编译链路 | `done` |
| 07 | `roku-execution-graph-builder` / `roku-runtime-service` | 实现 dependency-aware scheduler、checkpoint 恢复和 DAG 就绪节点调度 | `done` |
| 08 | `roku-runtime-service` / `roku-state-store` | 实现 approval ticket 持久化、审批查询、审批决策与任务恢复 | `done` |
| 09 | `roku-capability-auth` | 完成 capability token 签发、校验、衰减和越权拒绝主链路 | `done` |
| 10 | `roku-validation-plane` | 完成 schema / semantic / provenance / policy 四层校验 baseline | `done` |
| 11 | `roku-state-store` | 完成 task / event / approval / result 仓储接口及内存/文件实现 | `done` |
| 12 | `roku-artifact-store` | 完成 artifact metadata 持久化、URI 查询和结果产物登记 | `done` |
| 13 | `roku-experiment-registry` | 完成 experiment run 生命周期、artifact 挂载和结果摘要持久化 | `done` |
| 14 | `roku-runtime-service` / `roku-validation-plane` | 让 validation 消费 artifact-backed evidence，而不是只依赖裸结果 | `done` |
| 15 | `roku-runtime-service` | 拆分 runtime service 根文件，分离 execution / data-plane 责任 | `done` |
| 16 | `roku-api-gateway` | 完成 request / approval HTTP 路由与 runtime executor 对接 | `done` |
| 17 | `roku-api-gateway` | 完成 task artifact / experiment 查询路由与响应模型 | `done` |
| 18 | `roku-cmd` / `roku-runtime-service` | 完成 CLI 到 runtime service 的可复用执行入口 | `done` |
| 19 | `roku-mcp-bridge` / `roku-coding-provider-adapter` | 完成外部 Coding MCP Provider 的基础契约和接入边界 | `done` |
| 20 | `roku-connectors-telegram` | 将 Telegram connector 从空壳推进到真实 request/response adapter | `done` |
| 21 | `roku-execution-graph-builder` / `roku-common-types` | 为 `PlanStep`、`TaskNode`、`TaskGraph` 增加 branch/join 语义与依赖元数据 | `done` |
| 22 | `roku-runtime-service` / `roku-common-types` | 引入显式 `NodeResultSet`、`JoinPolicy`、`AggregationMode` 等聚合合同 | `done` |
| 23 | `roku-runtime-service` | 在 aggregation / validation 节点中真正消费 join policy，而不是隐式遍历结果 | `done` |
| 24 | `roku-tool-runtime` | 完善 tool descriptor、sandbox profile、timeout/retry 和 deterministic execution hooks | `done` |
| 25 | `roku-state-store` | 增加 PostgreSQL task/event/result backend，并抽象生产级 persistence boundary | `todo` |
| 26 | `roku-state-store` / dispatch plane | 增加 Redis / NATS / JetStream 风格 dispatch 与 backpressure 抽象 | `todo` |
| 27 | `roku-observability` | 增加 trace correlation、audit export、planning metrics 和 artifact/experiment telemetry | `done` |
| 28 | `roku-agent-instance-factory` / `roku-agent-runtime` | 完善 capability-based profile 装配、policy binding 和动态 worker 构造 | `done` |
| 29 | `roku-planning-engine` / `roku-task-planner` | 增加 iterative refinement、tree search 轮次控制与 critique loop 钩子 | `done` |
| 30 | `roku-api-gateway` / data plane | 增加 artifact content/download 端点与更细粒度错误映射 | `todo` |
| 31 | `roku-llm-adapter` | 新增多 provider 模型路由与预算/风险感知调用接口 | `todo` |
| 32 | `roku-skill-registry` | 新增 skill descriptor、版本化、验证、发布和熔断生命周期 | `todo` |
| 33 | `roku-agent-directory` / A2A layer | 新增 AgentIdentity、CapabilityCard、WorkContract、DelegationTicket 与 discovery | `todo` |
| 34 | `roku-runtime-service` / org policy | 实现组织级审批策略、风险动作 gating 和 capability/approval 联动 | `todo` |
| 35 | `quant domain pack` | 增加量化研究 capability profiles、artifact schema 和 experiment reproducibility 扩展 | `todo` |
| 36 | `roku-e2e` / test matrix | 扩大 integration / property / chaos / replay 测试覆盖 | `todo` |
