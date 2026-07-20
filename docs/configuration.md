# Configuration

vesc-mcp reads its configuration when the process starts. Environment
variables override the configuration file, and the configuration file
overrides built-in defaults. Restart the server after a change.

Most users need only a package root for stdio, or no configuration at all for
local Streamable HTTP knowledge search.

## Configuration file

The default file on Ubuntu and macOS is:

```text
$HOME/.config/vesc-mcp/config.toml
```

On Windows, choose a location and set `VESC_MCP_CONFIG` to its full path in
the environment used to start the server. This avoids relying on a Unix-style
home-directory convention.

A practical starting file is:

```toml
[paths]
package_roots = ["/path/to/vesc-packages"]
vesc_tool = "/path/to/vesc_tool"

[features]
enable_flash = false

[knowledge]
mode = "lexical"

[feedback]
path = ".vesc-mcp-feedback"
writes_enabled = false
```

Windows paths may use forward slashes:

```toml
[paths]
package_roots = ["C:/VESC/packages"]
vesc_tool = "C:/VESC/Tool/vesc_tool.exe"
```

The repository also includes [`config.example.toml`](../config.example.toml).

## Package access

`package_roots` lists the directories the stdio server may scan, inspect,
check, and build. Paths outside these directories are rejected.

When the connected MCP client advertises filesystem roots, package tools also
use its local `file://` roots for that connection. This lets a persistent
launchd/systemd server access the active Codex or Claude project checkout
without putting that project path in the daemon configuration. Clients that
do not advertise roots continue to use only `package_roots`.

| Setting | Environment variable | Default |
|---------|----------------------|---------|
| `[paths] package_roots` | `VESC_PACKAGE_ROOTS` | no allowed package directories |

On Ubuntu and macOS, multiple environment roots may be separated by commas or
colons:

```bash
export VESC_PACKAGE_ROOTS="/path/to/packages,/another/package-root"
```

On Windows, use `package_roots` in `config.toml`; a drive-letter colon is
ambiguous in `VESC_PACKAGE_ROOTS`.

Authenticated HTTP clients can use package tools. They are sandboxed to the
configured roots plus that client's advertised local `file://` roots; an
unauthenticated HTTP connection does not expose package tools.

## Building packages

`build_vescpkg` calls the official VESC Tool command-line executable.

| Setting | Environment variable | Default |
|---------|----------------------|---------|
| `[paths] vesc_tool` | `VESC_TOOL_PATH` | `vesc_tool` from `PATH` |

Set this only if the executable is not already on `PATH`. This setting does
not grant device access; vesc-mcp uses it only to build a local package.

## Release support files

Release archives contain a `catalog` directory alongside the server. Keep the
archive contents together. If you start the executable directly, set
`VESC_MCP_WORKSPACE_ROOT` to the extracted release directory.

| Variable | Purpose |
|----------|---------|
| `VESC_MCP_WORKSPACE_ROOT` | Directory containing the bundled `catalog` |

Source checkouts are discovered automatically when the server is run from the
project.

## Streamable HTTP

Run `vesc-mcp-server --http` to start a shared endpoint. The default is local
only at `http://127.0.0.1:8080/mcp`.

| Variable | Default | Purpose |
|----------|---------|---------|
| `VESC_MCP_HTTP_BIND` | `127.0.0.1:8080` | Listen address and port |
| `VESC_MCP_HTTP_PATH` | `/mcp` | Endpoint path |
| `VESC_MCP_HTTP_ALLOWED_HOSTS` | `localhost,127.0.0.1,::1` | Accepted Host values |
| `VESC_MCP_HTTP_ALLOWED_ORIGINS` | empty | Accepted browser origins |
| `VESC_MCP_HTTP_AUTH_TOKEN` | unset | Required bearer token when set |

See [http.md](http.md) for complete local and remote examples. Remote access
requires a TLS boundary, authentication, explicit host/origin policy, and a
firewall rule.

## Knowledge search

The default `lexical` mode is local and does not download a model.

| Setting | Environment variable | Default | Purpose |
|---------|----------------------|---------|---------|
| `[knowledge] mode` | `VESC_RAG_MODE` | `lexical` | Retrieval mode |
| `[knowledge] artifact_path` | `VESC_RAG_ARTIFACT` | bundled or embedded corpus | Generated artifact directory |
| `[knowledge] data_root` | `STATE_DIRECTORY` fallback | platform application-data directory | Persistent repository, snapshot, and artifact state |
| `[knowledge.semantic] model_dir` | `VESC_RAG_SEMANTIC_MODEL_DIR` | unset | Pinned local model directory |
| `[knowledge.semantic] model_id` | `VESC_RAG_SEMANTIC_MODEL_ID` | unset | Model identity recorded by the artifact |
| `[knowledge.semantic] model_revision` | `VESC_RAG_SEMANTIC_MODEL_REVISION` | unset | Pinned model revision |
| `[knowledge.semantic] max_length` | `VESC_RAG_SEMANTIC_MAX_LENGTH` | model profile | Optional lower input-length limit shared with artifact ingestion |
| `[knowledge.semantic] idle_timeout_secs` | `VESC_RAG_SEMANTIC_IDLE_TIMEOUT_SECS` | `300` | Seconds before unloading an idle model |
| `[feedback] path` | `VESC_RAG_FEEDBACK_PATH` | unset | Durable directory containing the bounded `feedback.json` store |
| `[feedback] writes_enabled` | `VESC_RAG_FEEDBACK_WRITES` | `false` | Expose model feedback write tools when a store is configured |

Feedback reads are available whenever `path` is configured. Write tools require
both `path` and `writes_enabled = true`. Streamable HTTP writes also require the
existing `VESC_MCP_HTTP_AUTH_TOKEN` boundary; otherwise HTTP remains read-only.

Supported modes:

| Mode | Behavior |
|------|----------|
| `lexical` | Offline keyword and identifier search; recommended default |
| `legacy` | Compatibility search for older results |
| `auto` | Uses hybrid search when configured; otherwise returns lexical results with a warning |
| `hybrid` | Requires a compatible local vector artifact and model; reports an error if unavailable |

The server never downloads a semantic model at startup. Model directory,
identity, and revision must match the vector artifact manifest.

### Managed knowledge repositories

`[[knowledge.repositories]]` declares approved Git sources without requiring a
source checkout beside the executable. Configuration is validated before any
filesystem access. Repository IDs are stable lowercase path-safe identifiers;
remotes must be credential-free HTTPS URLs; refs must be full `refs/...`
names; and include/exclude rules must be relative patterns without `..` path
components. Duplicate IDs and zero or inconsistent source limits are rejected.

```toml
[knowledge]
data_root = "/var/lib/vesc-mcp"

[[knowledge.repositories]]
id = "vesc-tool"
remote_url = "https://github.com/vedderb/vesc_tool.git"
default_ref = "refs/heads/master"
policy = "required"
include = ["**/*.cpp", "**/*.h", "*.pro"]
exclude = ["build/**"]
trust_tier = "official"
license = "GPL-3.0-or-later"
attribution = "VESC Project"
max_file_bytes = 1048576
max_files = 100000
max_total_bytes = 1073741824

[[knowledge.repositories]]
id = "vesc-pkg"
remote_url = "https://github.com/vedderb/vesc_pkg.git"
default_ref = "refs/heads/main"
policy = "optional"
include = ["**/*.lisp", "**/*.md", "**/*.qml"]
exclude = [".git/**"]
trust_tier = "official"
license = "GPL-3.0-or-later"
attribution = "VESC Project"
max_file_bytes = 1048576
max_files = 100000
max_total_bytes = 1073741824
```

Repository order in runtime configuration is deterministic by `id`. An empty
repository list remains valid and preserves embedded or explicitly configured
artifact retrieval.

The application data root resolves in this order:

1. absolute `[knowledge] data_root`;
2. systemd `STATE_DIRECTORY`;
3. `XDG_DATA_HOME/vesc-mcp`;
4. the platform user-data location (`~/.local/share/vesc-mcp` on Linux,
   `~/Library/Application Support/vesc-mcp` on macOS, or
   `%LOCALAPPDATA%/vesc-mcp` on Windows).

The internal layout is portable and derived only from validated IDs:

```text
repositories/<id>.git/
repositories/<id>.refs.json
snapshots/<snapshot-id>.json
artifacts/<snapshot-id>/
artifacts/<snapshot-id>/history.json
preparation-status.json
tmp/
```

In HTTP mode the server binds first, then clones or refreshes enabled bare
repositories and prepares the default artifact in the background. The `ping`
response exposes the bounded `knowledge` state and phase plus completed/total
repository counts; the same state is atomically shared through
`preparation-status.json`. The default artifact is one combined history
containing every commit reachable from each configured default branch. It
stores changed path occurrences separately from content-addressed passages, so
unchanged blobs and chunks are not duplicated for every commit. Binary changes
remain history occurrences but do not become searchable text chunks. Tags and
branches are retained as named aliases into the commit graph.

A later refresh walks the current graph with `gix`, reuses commits and passages
from the previous immutable generation, ingests only newly reachable changed
blobs, validates the new artifact, and then atomically advances the mutable
default alias. Explicit prewarm selections remain commit-tree snapshots for
version-specific comparisons. A failed refresh keeps the last complete default
and reports it as stale. Run `vesc-mcp-server --refresh-repositories` from a
deployment hook or timer to perform an incremental refresh and exit. There is
no built-in background scheduler.

Agents discover this local state with the read-only
`list_vesc_source_versions` tool before choosing evidence. It accepts optional
repository IDs, `default`/`branch`/`tag` kinds, a name/ref prefix, and a bounded
cursor page. Results contain sanitized remote identities, full refs, peeled
commit IDs, fresh/stale availability, and whether a commit is already present
in the default or a prewarmed snapshot. Discovery never fetches and never
returns repository paths or raw Git transport errors. The intended workflow is
list versions, select or confirm exact refs, prepare the version set, then
search that immutable snapshot.

`tmp/` is inside the data root so clone staging can atomically rename on the
same filesystem. Git network operations and directory creation belong to the
repository-store lifecycle; parsing this configuration does neither. Semantic model files remain independently configured with
`[knowledge.semantic] model_dir` and are never downloaded implicitly.

On the measured Ryzen 5 8600G + RX 5700 XT development host only, the server
selects the pinned Jina code INT8 query model automatically after the matching
FP16 artifact has been built. Any explicit knowledge mode, artifact, or
semantic-model setting takes precedence.

Search is bounded to a 4 KiB query, 50 results, 8 KiB per passage, and a 64
KiB serialized response by default. These file-only limits can be adjusted:

```toml
[knowledge]
max_limit = 50
max_query_bytes = 4096
max_response_bytes = 65536
max_passage_bytes = 8192
```

## Optional source checkouts

Most users do not need upstream source repositories. They are used only for
catalog validation, knowledge artifact generation, and detailed source
attribution.

| Config key | Environment variable | Purpose |
|------------|----------------------|---------|
| `[paths] refloat_root` | `VESC_REFLOAT_ROOT` | Refloat source checkout |
| `[paths] vesc_root` | `VESC_ROOT` | VESC firmware source checkout |
| `[paths] poc_root` | `VESC_POC_ROOT` | Rust proof-of-concept checkout |
| `[paths] vesc_tool_root` | `VESC_VESC_TOOL_ROOT` | VESC Tool source checkout |

Use paths in `config.toml` or environment variables. Do not put personal
absolute paths in shared client configurations or documentation.

## Logging

Set `RUST_LOG=info` for normal diagnostics or a narrower filter such as
`vesc_mcp_core=debug` for detailed troubleshooting. Logs go to stderr so they
do not corrupt stdio MCP messages.

## Reserved flash setting

`VESC_MCP_ENABLE_FLASH` and `[features] enable_flash` are reserved for a
possible future feature. They default to false, and setting them currently
adds no upload or flash tools. See [safety.md](safety.md).
