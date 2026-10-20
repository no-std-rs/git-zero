using Workerd = import "/workerd/workerd.capnp";

# The same bounded Wasm/HTTP implementation as Wrangler, with local SQLite and
# an actual R2 S3 bucket. Serve behind TLS; this socket defaults to loopback.
const config :Workerd.Config = (
  services = [
    (name = "repository", worker = (
      modules = [
        (name = "src/streamed.mjs", esModule = embed "src/streamed.mjs"),
        (name = "src/codec.mjs", esModule = embed "src/codec.mjs"),
        (name = "src/storage.mjs", esModule = embed "src/storage.mjs"),
        (name = "src/r2-s3.mjs", esModule = embed "src/r2-s3.mjs"),
        (name = "src/intake.mjs", esModule = embed "src/intake.mjs"),
        (name = "src/source.mjs", esModule = embed "src/source.mjs"),
        (name = "src/publication.mjs", esModule = embed "src/publication.mjs"),
        (name = "src/fetch.mjs", esModule = embed "src/fetch.mjs"),
        (name = "src/response.mjs", esModule = embed "src/response.mjs"),
        (name = "build/git_zero_host_wasm.wasm", wasm = embed "build/git_zero_host_wasm.wasm"),
      ],
      compatibilityDate = "2026-10-06",
      bindings = [
        (name = "REPOSITORIES", durableObjectNamespace = "OwnRepository"),
        (name = "HOST_TOKEN", fromEnvironment = "HOST_TOKEN"),
        (name = "R2_S3_URL", fromEnvironment = "R2_S3_URL"),
        (name = "R2_ACCESS_KEY_ID", fromEnvironment = "R2_ACCESS_KEY_ID"),
        (name = "R2_SECRET_ACCESS_KEY", fromEnvironment = "R2_SECRET_ACCESS_KEY"),
        (name = "REPO_FORMAT", fromEnvironment = "REPO_FORMAT"),
        (name = "TEST_MODE", fromEnvironment = "TEST_MODE"),
      ],
      durableObjectNamespaces = [
        (className = "OwnRepository", uniqueKey = "git-zero-own-repository-v1", enableSql = true),
      ],
      durableObjectStorage = (localDisk = "state"),
    )),
    (name = "state", disk = (writable = true)),
    (name = "internet", network = (allow = ["public", "private"])),
  ],
  sockets = [(name = "http", address = "127.0.0.1:8787", http = (), service = "repository")],
);
