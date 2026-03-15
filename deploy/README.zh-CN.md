# 部署说明

English version: [README.md](./README.md)

当前 `deploy/` 目录用于存放 Docker 部署相关资源。如果后续增加 Kubernetes 清单，建议统一放到
`deploy/k8s/` 下，避免和容器构建文件混放。

## 前置工具

- Docker
- Docker Buildx

如果需要显式指定目标平台，或者需要构建多架构镜像，就需要使用 `buildx`。

在较新的 macOS Docker 环境中，`buildx` 通常已经内置。可以先检查一下：

```bash
docker buildx version
```

## 构建方式

所有构建命令都需要在仓库根目录执行，这样才会正确使用根目录下的 `.dockerignore`。

### 不使用 Buildx 的单架构构建

如果你只需要构建当前宿主机默认架构的镜像，可以直接使用 `docker build`。

常见情况：

- Apple Silicon 的 macOS 上通常会得到 `linux/arm64`
- x86 Linux 主机上通常会得到 `linux/amd64`

```bash
docker build -f deploy/Dockerfile -t roku:local .
```

这是本地做基础冒烟验证时最简单的方式。

### 使用 Buildx 的单架构构建

如果你希望明确指定目标架构，即使只构建单一平台，也建议使用 `buildx`。

构建 `linux/arm64`，并把镜像加载到本地 Docker 镜像仓库：

```bash
docker buildx build \
  --platform linux/arm64 \
  -f deploy/Dockerfile \
  -t roku:arm64-local \
  --load \
  .
```

构建 `linux/amd64`，并加载到本地：

```bash
docker buildx build \
  --platform linux/amd64 \
  -f deploy/Dockerfile \
  -t roku:amd64-local \
  --load \
  .
```

这里的 `--load` 适合单平台构建，因为构建完成后可以直接在本机 `docker run`。

### 使用 Buildx 的多架构构建

如果你需要一个同时支持 `linux/amd64` 和 `linux/arm64` 的统一 tag，就需要使用 `buildx`
做多架构构建。

推送真正的多架构镜像到远端仓库：

```bash
docker buildx build \
  --platform linux/amd64,linux/arm64 \
  -f deploy/Dockerfile \
  -t your-registry/roku:latest \
  --push \
  .
```

如果只是本地校验多架构构建是否成功，不推送镜像，可以这样做：

```bash
docker buildx build \
  --platform linux/amd64,linux/arm64 \
  -f deploy/Dockerfile \
  --output=type=cacheonly \
  .
```

## 在 macOS 上如何做多架构构建

这个仓库的多架构本地验证，就是在 macOS 上按下面的方式完成的。

如果直接执行：

```bash
docker buildx build --platform linux/amd64,linux/arm64 -f deploy/Dockerfile --output=type=cacheonly .
```

报错提示默认 `docker` driver 不支持 multi-platform build，那么就创建一个临时的
`docker-container` builder，并先做初始化：

```bash
docker buildx create --name roku-multiarch --driver docker-container --use
docker buildx inspect --bootstrap
```

然后显式使用这个 builder 做多架构构建校验：

```bash
docker buildx build \
  --builder roku-multiarch \
  --platform linux/amd64,linux/arm64 \
  -f deploy/Dockerfile \
  --output=type=cacheonly \
  .
```

如果要直接发布多架构镜像，同样沿用这个 builder，只是把输出方式改成 `--push`：

```bash
docker buildx build \
  --builder roku-multiarch \
  --platform linux/amd64,linux/arm64 \
  -f deploy/Dockerfile \
  -t your-registry/roku:latest \
  --push \
  .
```

验证完成后，可以把临时 builder 删掉：

```bash
docker buildx rm roku-multiarch
```

在 macOS 上，这种方式本质上是通过 BuildKit 和 `docker-container` builder 提供的能力，
让一台机器可以完成 `linux/amd64` 与 `linux/arm64` 的多架构构建验证。

## 镜像内容

当前镜像内包含：

- `roku-cmd` 的 release 二进制
- `config/` 下的运行时配置文件
- 面向容器的默认环境变量：
  - `ROKU_HOME=/app/.roku`
  - `ROKU_API_BIND_ADDR=0.0.0.0:8787`

## 运行方式

执行一次确定性请求：

```bash
docker run --rm roku:local once "帮我确认当前运行时是否正常"
```

启动 HTTP 网关，并把运行时数据持久化到宿主机：

```bash
docker run --rm \
  -p 8787:8787 \
  -v "$(pwd)/.roku-container:/app/.roku" \
  -e OPENROUTER_API_KEY=your-key \
  roku:local \
  api-gateway
```

如果只是想看支持的命令：

```bash
docker run --rm roku:local --help
```

## 注意事项

- 构建上下文必须是仓库根目录：`docker build -f deploy/Dockerfile .`
- `.dockerignore` 必须放在仓库根目录，配合上述构建上下文才会生效
- 只做当前架构本地测试时，普通 `docker build` 就够用
- 需要显式指定架构，或者需要多架构镜像时，必须使用 `docker buildx build`
- 镜像默认使用非 root 用户运行，可写数据默认放在 `/app/.roku`
- 容器内 `api-gateway` 默认监听 `8787` 端口
