# Deploy

Chinese version: [README.zh-CN.md](./README.zh-CN.md)

`deploy/` currently contains Docker deployment assets. If Kubernetes manifests are added later,
they should live under `deploy/k8s/` so container assets and cluster manifests do not get mixed.

## Prerequisites

- Docker
- Docker Buildx for explicit target-platform or multi-architecture builds

On recent macOS Docker environments, `buildx` is usually already available. You can verify it with:

```bash
docker buildx version
```

## Build Modes

All build commands must run from the repository root so Docker can use the root-level
`.dockerignore`.

### Single-Architecture Build Without Buildx

Use plain `docker build` when you only need the current builder architecture.

Examples:

- on Apple Silicon macOS, this is usually `linux/arm64`
- on an x86 Linux host, this is usually `linux/amd64`

```bash
docker build -f deploy/Dockerfile -t roku:local .
```

This is the simplest choice for local smoke tests.

### Single-Architecture Build With Buildx

Use `buildx` when you want to force a specific target architecture, even for a single-platform
image.

Build an explicit `linux/arm64` image and load it into the local Docker image store:

```bash
docker buildx build \
  --platform linux/arm64 \
  -f deploy/Dockerfile \
  -t roku:arm64-local \
  --load \
  .
```

Build an explicit `linux/amd64` image and load it locally:

```bash
docker buildx build \
  --platform linux/amd64 \
  -f deploy/Dockerfile \
  -t roku:amd64-local \
  --load \
  .
```

`--load` is appropriate for single-platform builds when you want to run the image locally after
the build.

### Multi-Architecture Build With Buildx

Use `buildx` for a single tag that supports both `linux/amd64` and `linux/arm64`.

Push a real multi-architecture image to a registry:

```bash
docker buildx build \
  --platform linux/amd64,linux/arm64 \
  -f deploy/Dockerfile \
  -t your-registry/roku:latest \
  --push \
  .
```

Validate a multi-architecture build without pushing:

```bash
docker buildx build \
  --platform linux/amd64,linux/arm64 \
  -f deploy/Dockerfile \
  --output=type=cacheonly \
  .
```

## macOS Multi-Architecture Build Notes

This is how multi-architecture validation was done on macOS for this repository.

If `docker buildx build --platform linux/amd64,linux/arm64 ...` reports that the default
`docker` driver does not support multi-platform builds, create a temporary `docker-container`
builder and bootstrap it:

```bash
docker buildx create --name roku-multiarch --driver docker-container --use
docker buildx inspect --bootstrap
```

Then run the multi-architecture build with that builder:

```bash
docker buildx build \
  --builder roku-multiarch \
  --platform linux/amd64,linux/arm64 \
  -f deploy/Dockerfile \
  --output=type=cacheonly \
  .
```

For a registry push, keep the same builder and switch to `--push`:

```bash
docker buildx build \
  --builder roku-multiarch \
  --platform linux/amd64,linux/arm64 \
  -f deploy/Dockerfile \
  -t your-registry/roku:latest \
  --push \
  .
```

After validation, remove the temporary builder:

```bash
docker buildx rm roku-multiarch
```

On macOS, this build path relies on BuildKit plus emulation support provided through the
`docker-container` builder, so one machine can validate both `linux/amd64` and `linux/arm64`.

## Image Contents

This image bundles:

- the `roku-cmd` release binary
- runtime config files from `config/`
- container-friendly defaults:
  - `ROKU_HOME=/app/.roku`
  - `ROKU_API_BIND_ADDR=0.0.0.0:8787`

## Run

Run a one-shot deterministic request:

```bash
docker run --rm roku:local once "帮我确认当前运行时是否正常"
```

Run the HTTP gateway and persist runtime state to the host:

```bash
docker run --rm \
  -p 8787:8787 \
  -v "$(pwd)/.roku-container:/app/.roku" \
  -e OPENROUTER_API_KEY=your-key \
  roku:local \
  api-gateway
```

If you only want to inspect supported commands:

```bash
docker run --rm roku:local --help
```

## Notes

- The build context must be the repository root: `docker build -f deploy/Dockerfile .`
- `.dockerignore` must stay at the repository root to take effect with that build context
- Plain `docker build` is enough for current-architecture local testing
- `docker buildx build` is required for explicit target-platform builds and multi-architecture builds
- The image defaults to a non-root user and stores writable runtime data under `/app/.roku`
- `api-gateway` listens on port `8787` in the container by default
